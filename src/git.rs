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

// The uniform contract above is stated once rather than repeated on thirty
// functions, where it would train readers to skip `# Errors` sections.
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

    /// Trimmed stdout, or an error carrying git's own message.
    fn stdout_or_error(self, operation: &str) -> anyhow::Result<String> {
        self.stdout_verbatim_or_error(operation)
            .map(|out| out.trim().to_string())
    }

    /// Stdout exactly as git produced it, or an error carrying git's own
    /// message. For file contents, where a trailing newline is part of the
    /// file rather than noise.
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
/// A job is cut from a ref the user names, so the ref has to be resolved once
/// and the resulting commit used everywhere after: a branch that moves
/// mid-job must not silently change what the job was based on.
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
/// none, and nearly every other operation needs one to start from.
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

/// Create `branch` at `start_point` and check it out into a new worktree at
/// `path`. The repository's own working tree is left alone.
pub async fn add_worktree(
    repo: impl AsRef<Path>,
    path: impl AsRef<Path>,
    branch: &str,
    start_point: &str,
) -> anyhow::Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let path_arg = path.to_string_lossy().into_owned();

    run_expecting_success(
        repo,
        &["worktree", "add", "-b", branch, &path_arg, start_point],
        &format!("worktree add {branch}"),
    )
    .await
    .map(|_| ())
}

/// Check an existing branch out into a new worktree, rather than creating the
/// branch. Used when a run's branch outlived its checkout — `gc` removed it,
/// or a crash did.
pub async fn add_worktree_for_existing_branch(
    repo: impl AsRef<Path>,
    path: impl AsRef<Path>,
    branch: &str,
) -> anyhow::Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let path_arg = path.to_string_lossy().into_owned();

    run_expecting_success(
        repo,
        &["worktree", "add", &path_arg, branch],
        &format!("worktree add {branch}"),
    )
    .await
    .map(|_| ())
}

/// Delete a branch whether or not it was merged.
///
/// Only ever called on a branch assembly-line created for a job attempt that
/// a later attempt supersedes; the force is what makes an unmerged failed
/// attempt collectable.
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
///
/// A repository with no remote is an ordinary local run, not a fault: the
/// caller keeps the job's branch as a local ref instead of publishing it.
pub async fn remote_exists(repo: impl AsRef<Path>, remote: &str) -> anyhow::Result<bool> {
    let configured = run_expecting_success(repo, &["remote"], "remote").await?;
    Ok(configured.lines().map(str::trim).any(|name| name == remote))
}

/// Push `branch` to `remote`.
///
/// Deliberately without `--set-upstream`: that writes `branch.*.remote` into
/// the repository's config, and the target repository is never modified. A
/// later round pushes the same branch name again and fast-forwards without it.
pub async fn push_branch(repo: impl AsRef<Path>, remote: &str, branch: &str) -> anyhow::Result<()> {
    run_expecting_success(
        repo,
        &["push", remote, branch],
        &format!("push {remote} {branch}"),
    )
    .await
    .map(|_| ())
}

/// Push an explicit `<src>:<dst>` refspec.
///
/// How work lands directly on a base branch: the remote's ref is advanced to
/// the run branch. Deliberately not forced — a rejected non-fast-forward means
/// the base moved and this work has not seen it, which is worth stopping for.
pub async fn push_refspec(
    repo: impl AsRef<Path>,
    remote: &str,
    refspec: &str,
) -> anyhow::Result<()> {
    run_expecting_success(
        repo,
        &["push", remote, refspec],
        &format!("push {remote} {refspec}"),
    )
    .await
    .map(|_| ())
}

/// Remove a worktree and its administrative entry. Forcing is deliberate: the
/// worktree is assembly-line's to discard, and it routinely holds untracked
/// build output.
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
/// Beyond the usual git failures, this returns an error if a `never_commit`
/// path is tracked after the commit — meaning the agent committed it itself.
/// That is treated as fatal because the alternative is leaking a seeded
/// credential onto a branch bound for a remote.
pub async fn commit_all_except(
    worktree: impl AsRef<Path>,
    message: &str,
    never_commit: &[String],
) -> anyhow::Result<Option<String>> {
    let worktree = worktree.as_ref();
    run_expecting_success(worktree, &["add", "-A"], "add -A").await?;

    // Unstaging a path that was never staged is a harmless no-op.
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

/// Fail loudly if a path that must never be committed ended up tracked.
///
/// Unstaging covers commits assembly-line makes; this catches the case where
/// the agent committed the file itself. For a seeded credential, a failed job
/// is far better than a silent leak onto a branch bound for a remote.
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

/// Numeric diff of a worktree's HEAD against `base`, for reporting at a gate.
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

/// Paths tracked on the current branch that match `candidates`.
///
/// Used to assert that seeded files never entered history.
pub async fn tracked_among(
    worktree: impl AsRef<Path>,
    candidates: &[String],
) -> anyhow::Result<Vec<String>> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let args: Vec<&str> = ["ls-files", "--"]
        .into_iter()
        .chain(candidates.iter().map(String::as_str))
        .collect();

    let listed = run_expecting_success(worktree, &args, "ls-files").await?;
    Ok(listed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(String::from)
        .collect())
}
