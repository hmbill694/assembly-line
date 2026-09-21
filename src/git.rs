//! Git operations, run as subprocesses.
//!
//! Two rules shape this module. The user's working tree is never touched — all
//! writes happen in worktrees assembly-line creates elsewhere. And every
//! operation names the repository or worktree it acts on, so nothing depends
//! on the process's current directory.
//!
//! # Errors
//!
//! Every function here shares one failure mode: `git` could not be spawned, or
//! it exited non-zero, in which case the error carries git's own stderr. Only
//! the functions whose failure means something *beyond* that document it
//! individually.

#![allow(clippy::missing_errors_doc)]

use std::path::Path;
use tokio::process::Command;

/// A finished `git` invocation, before deciding whether its status matters.
#[derive(Debug, Clone)]
pub struct GitOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl GitOutput {
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.exit_code == 0
    }

    fn stdout_or_error(self, operation: &str) -> anyhow::Result<String> {
        self.stdout_verbatim_or_error(operation)
            .map(|out| out.trim().to_string())
    }

    /// For file contents, where a trailing newline is part of the file rather
    /// than noise.
    fn stdout_verbatim_or_error(self, operation: &str) -> anyhow::Result<String> {
        match self.succeeded() {
            true => Ok(self.stdout),
            false => Err(anyhow::anyhow!(
                "{operation} failed (exit {}): {}",
                self.exit_code,
                self.stderr.trim()
            )),
        }
    }
}

/// Run `git` in `dir`, leaving the caller to judge the result.
pub async fn run_allowing_failure(
    dir: impl AsRef<Path>,
    args: &[&str],
) -> anyhow::Result<GitOutput> {
    let output = Command::new("git")
        .args(args)
        // Machine-readable formats (`--porcelain`, `--numstat`) are stable, but
        // pinning the locale keeps any incidental output predictable too.
        .env("LC_ALL", "C")
        .current_dir(dir.as_ref())
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("running `git {}`: {e}", args.join(" ")))?;

    Ok(GitOutput {
        exit_code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

async fn run_expecting_success(
    dir: impl AsRef<Path>,
    args: &[&str],
    operation: &str,
) -> anyhow::Result<String> {
    run_allowing_failure(dir, args)
        .await?
        .stdout_or_error(operation)
}

pub async fn head_sha(repo: impl AsRef<Path>) -> anyhow::Result<String> {
    run_expecting_success(repo, &["rev-parse", "HEAD"], "rev-parse HEAD").await
}

/// The commit `git_ref` names — a branch, a tag, `HEAD`, or a raw sha.
///
/// This pins the checkout to one commit rather than handing git the ref name
/// and letting it re-resolve later. It does not, on its own, make the whole
/// job atomic with respect to the ref: [`RepoConfig::from_ref`] resolves the
/// same ref name earlier, to read config, and this resolves it again when the
/// checkout starts. A branch that moves in between gives config from one
/// commit and a tree from another — a window this function does not close.
///
/// [`RepoConfig::from_ref`]: crate::config::RepoConfig::from_ref
pub async fn sha_at_ref(repo: impl AsRef<Path>, git_ref: &str) -> anyhow::Result<String> {
    run_expecting_success(
        repo,
        &["rev-parse", &format!("{git_ref}^{{commit}}")],
        &format!("rev-parse {git_ref}"),
    )
    .await
}

/// One file's contents as of `git_ref`, or `None` when that ref does not carry
/// it.
///
/// Reading configuration from a ref rather than from a checkout is what stops
/// a job editing the settings that govern it: the agent's branch can say
/// anything, and this never looks at it.
pub async fn file_at_ref(
    repo: impl AsRef<Path>,
    git_ref: &str,
    path: &str,
) -> anyhow::Result<Option<String>> {
    let spec = format!("{git_ref}:{path}");
    let present = run_allowing_failure(&repo, &["cat-file", "-e", &spec])
        .await?
        .succeeded();

    match present {
        false => Ok(None),
        // Deliberately not `run_expecting_success`, which trims: a config file
        // read back must be the bytes the ref carries, not a tidied copy.
        true => run_allowing_failure(&repo, &["show", &spec])
            .await?
            .stdout_verbatim_or_error(&format!("show {spec}"))
            .map(Some),
    }
}

/// The commit `branch` points at. A revise round starts here, so the agent
/// sees its own prior work rather than starting over.
pub async fn branch_tip(repo: impl AsRef<Path>, branch: &str) -> anyhow::Result<String> {
    run_expecting_success(
        repo,
        &["rev-parse", &format!("refs/heads/{branch}")],
        &format!("rev-parse {branch}"),
    )
    .await
}

/// The checked-out branch, or `None` when HEAD is detached.
pub async fn current_branch(repo: impl AsRef<Path>) -> anyhow::Result<Option<String>> {
    let name =
        run_expecting_success(repo, &["branch", "--show-current"], "branch --show-current").await?;
    Ok((!name.is_empty()).then_some(name))
}

/// Whether the repository has any commits yet. A freshly initialised repo has
/// none, and a job needs one to branch from.
pub async fn has_commits(repo: impl AsRef<Path>) -> anyhow::Result<bool> {
    Ok(
        run_allowing_failure(repo, &["rev-parse", "--verify", "HEAD"])
            .await?
            .succeeded(),
    )
}

pub async fn branch_exists(repo: impl AsRef<Path>, branch: &str) -> anyhow::Result<bool> {
    Ok(run_allowing_failure(
        repo,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .await?
    .succeeded())
}

/// `git worktree add` creates the worktree directory itself, but not the path
/// leading to it.
fn make_room_for_worktree(path: &Path) -> std::io::Result<()> {
    match path.parent() {
        Some(parent) => std::fs::create_dir_all(parent),
        None => Ok(()),
    }
}

/// Where a new worktree's content begins, and what that makes of its branch.
///
/// Both variants name the commit the worktree starts at. `git worktree add`
/// needs it only when creating the branch — checking out a branch that already
/// exists lands at its tip by definition — but callers diff against it either
/// way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeStart {
    /// Create `branch` at this commit, superseding any earlier attempt's.
    CreatingBranch { at: String },
    /// Check out a branch that is already there, whose tip is this commit —
    /// how a revise round picks its own branch back up.
    OnExistingBranch { tip: String },
}

impl WorktreeStart {
    /// The commit the worktree starts from, however its branch came to be —
    /// which is what a round's diff is measured against.
    #[must_use]
    pub fn start_commit(&self) -> &str {
        match self {
            Self::CreatingBranch { at } => at,
            Self::OnExistingBranch { tip } => tip,
        }
    }
}

/// Check `branch` out into a new worktree at `path`, creating the branch first
/// when `start` says to.
pub async fn add_worktree(
    repo: impl AsRef<Path>,
    path: impl AsRef<Path>,
    branch: &str,
    start: &WorktreeStart,
) -> anyhow::Result<()> {
    let path = path.as_ref();
    make_room_for_worktree(path)?;
    let path_arg = path.to_string_lossy().into_owned();

    let args: Vec<&str> = match start {
        WorktreeStart::CreatingBranch { at } => {
            vec!["worktree", "add", "-b", branch, &path_arg, at]
        }
        WorktreeStart::OnExistingBranch { .. } => vec!["worktree", "add", &path_arg, branch],
    };

    run_expecting_success(repo, &args, &format!("worktree add {branch}"))
        .await
        .map(|_| ())
}

/// Delete a branch whether or not it was merged — a superseded attempt is
/// unmerged by definition, and a safe delete would refuse it.
pub async fn delete_branch(repo: impl AsRef<Path>, branch: &str) -> anyhow::Result<()> {
    run_expecting_success(
        repo,
        &["branch", "-D", branch],
        &format!("branch -D {branch}"),
    )
    .await
    .map(|_| ())
}

/// Whether `remote` is configured.
pub async fn remote_exists(repo: impl AsRef<Path>, remote: &str) -> anyhow::Result<bool> {
    let configured = run_expecting_success(repo, &["remote"], "remote").await?;
    Ok(configured.lines().map(str::trim).any(|name| name == remote))
}

/// Deliberately without `--set-upstream`: that would write `branch.*.remote`
/// into the repository's config. A later round pushes the same branch name
/// again and fast-forwards without it.
pub async fn push_branch(repo: impl AsRef<Path>, remote: &str, branch: &str) -> anyhow::Result<()> {
    run_expecting_success(
        repo,
        &["push", remote, branch],
        &format!("push {remote} {branch}"),
    )
    .await
    .map(|_| ())
}

/// Forcing is deliberate: the worktree is assembly-line's to discard, and it
/// routinely holds untracked build output.
pub async fn remove_worktree(repo: impl AsRef<Path>, path: impl AsRef<Path>) -> anyhow::Result<()> {
    let path_arg = path.as_ref().to_string_lossy().into_owned();
    run_expecting_success(
        repo,
        &["worktree", "remove", "--force", &path_arg],
        "worktree remove",
    )
    .await
    .map(|_| ())
}

/// Drop administrative entries for worktrees whose directories are gone.
///
/// Not optional: git still lists such a worktree and refuses to reuse its
/// path until told otherwise, so deleting the directory alone leaves the name
/// unusable.
pub async fn prune_worktrees(repo: impl AsRef<Path>) -> anyhow::Result<()> {
    run_expecting_success(repo, &["worktree", "prune"], "worktree prune")
        .await
        .map(|_| ())
}

/// Stage everything and commit. `None` means the tree was clean — a normal
/// outcome, since an agent may correctly conclude no change is needed.
pub async fn commit_all(
    worktree: impl AsRef<Path>,
    message: &str,
) -> anyhow::Result<Option<String>> {
    commit_all_except(worktree, message, &[]).await
}

/// Commit everything except `never_commit`, which stay on disk for the agent
/// to read but are kept out of history.
///
/// Staging is controlled directly rather than through `info/exclude`, because
/// git resolves that file from the repository's *common* directory — writing
/// it would modify the user's repository, and a per-worktree copy is ignored.
///
/// # Errors
///
/// Beyond the usual git failures, an error if a `never_commit` path is
/// tracked once the commit is made — meaning the agent committed it itself.
pub async fn commit_all_except(
    worktree: impl AsRef<Path>,
    message: &str,
    never_commit: &[String],
) -> anyhow::Result<Option<String>> {
    let worktree = worktree.as_ref();
    run_expecting_success(worktree, &["add", "-A"], "add -A").await?;

    // `run_allowing_failure`: unstaging a path that was never staged is a
    // no-op worth ignoring, not an error.
    for path in never_commit {
        run_allowing_failure(worktree, &["reset", "--quiet", "--", path]).await?;
    }

    let nothing_staged = run_allowing_failure(worktree, &["diff", "--cached", "--quiet"])
        .await?
        .succeeded();
    if nothing_staged {
        return Ok(None);
    }

    run_expecting_success(
        worktree,
        &["commit", "--no-verify", "-m", message],
        "commit",
    )
    .await?;
    ensure_untracked(worktree, never_commit).await?;
    head_sha(worktree).await.map(Some)
}

/// Unstaging covers the commits assembly-line makes; this catches the agent
/// committing a seeded file itself. Fatal, because a failed job is far better
/// than a leaked credential on a branch bound for a remote.
async fn ensure_untracked(worktree: &Path, never_commit: &[String]) -> anyhow::Result<()> {
    if never_commit.is_empty() {
        return Ok(());
    }

    let args: Vec<&str> = ["ls-files", "--"]
        .into_iter()
        .chain(never_commit.iter().map(String::as_str))
        .collect();
    let tracked = run_expecting_success(worktree, &args, "ls-files").await?;

    match tracked.is_empty() {
        true => Ok(()),
        false => Err(anyhow::anyhow!(
            "refusing to continue: seeded file(s) were committed by the agent: {}",
            tracked.lines().collect::<Vec<_>>().join(", ")
        )),
    }
}

/// Whether a worktree has uncommitted changes, tracked or otherwise.
pub async fn is_dirty(worktree: impl AsRef<Path>) -> anyhow::Result<bool> {
    let status = run_expecting_success(worktree, &["status", "--porcelain"], "status").await?;
    Ok(!status.is_empty())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DiffStat {
    pub files: usize,
    pub insertions: usize,
    pub deletions: usize,
}

pub async fn diff_stat_against(worktree: impl AsRef<Path>, base: &str) -> anyhow::Result<DiffStat> {
    let numstat = run_expecting_success(
        worktree,
        &["diff", "--numstat", &format!("{base}..HEAD")],
        "diff --numstat",
    )
    .await?;

    Ok(numstat.lines().filter(|line| !line.trim().is_empty()).fold(
        DiffStat::default(),
        |totals, line| {
            // "<added>\t<removed>\t<path>", where binary files report "-".
            let mut columns = line.split('\t');
            let added = columns.next().and_then(|c| c.parse::<usize>().ok());
            let removed = columns.next().and_then(|c| c.parse::<usize>().ok());
            DiffStat {
                files: totals.files + 1,
                insertions: totals.insertions + added.unwrap_or(0),
                deletions: totals.deletions + removed.unwrap_or(0),
            }
        },
    ))
}
