//! Git operations, run as subprocesses.
//!
//! Two rules shape this module. The user's working tree is never touched — all
//! writes happen in scratch clones assembly-line creates elsewhere. And every
//! operation names the repository or clone it acts on, so nothing depends on
//! the process's current directory.
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
        .kill_on_drop(true)
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

/// The commit `git_ref` names in this repository — a branch, a tag, `HEAD`,
/// or a raw sha. A job starts from the remote's copy of the ref instead
/// ([`pinned`]); this local answer is only compared against it.
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

/// A ref name and the commit it named when the job was planned.
///
/// The name is kept because a clone fetches by name; the sha is what the job
/// actually starts from, so config read at planning time and the tree the job
/// runs on are the same commit even if the ref moves in between.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PinnedRef {
    pub name: String,
    pub sha: String,
}

/// The URL a job clones `remote` from and pushes its branch back to, or
/// `None` when the repository has no such remote.
///
/// A relative path is resolved against `repo`: git reads it relative to the
/// repository, and from a scratch clone anywhere else it names nothing.
///
/// # Errors
///
/// When `remote` pushes somewhere the job's clone cannot know about: a
/// `remote.<name>.pushurl`, or a `url.<base>.pushInsteadOf` in the
/// repository's own config. The clone has the fetch URL and none of the
/// repository's config, so its push would go to the wrong place. A rewrite
/// in global or system config is no reason to refuse: the clone reads that
/// config too, and pushes where the user's own push would.
pub async fn remote_url(repo: impl AsRef<Path>, remote: &str) -> anyhow::Result<Option<String>> {
    let repo = repo.as_ref();
    let fetched = run_allowing_failure(repo, &["remote", "get-url", remote]).await?;
    let Some(fetch_url) = fetched
        .succeeded()
        .then(|| fetched.stdout.trim().to_string())
    else {
        return Ok(None);
    };
    let pushurl_key = format!("remote.{remote}.pushurl");
    let has_pushurl = run_allowing_failure(repo, &["config", "--get-all", &pushurl_key])
        .await?
        .succeeded();
    let local_rewrites = run_allowing_failure(
        repo,
        &[
            "config",
            "--local",
            "--includes",
            "--get-regexp",
            r"^url\..*\.pushinsteadof$",
        ],
    )
    .await?
    .stdout;
    let local_rewrite_of_this_url = local_rewrites
        .lines()
        .filter_map(|line| line.split_once(' '))
        .find(|(_, prefix)| fetch_url.starts_with(prefix))
        .map(|(key, _)| key.to_string());

    match (has_pushurl, local_rewrite_of_this_url) {
        (true, _) => Err(anyhow::anyhow!(
            "'{remote}' fetches from {fetch_url} but pushes elsewhere — a job clones from and \
             pushes back to one URL, so unset {pushurl_key} or point it at the same place"
        )),
        (false, Some(key)) => Err(anyhow::anyhow!(
            "'{remote}' fetches from {fetch_url} but {key} in this repository's config rewrites \
             where it pushes — a job's clone does not see this repository's config, so its push \
             would go to {fetch_url}; move the setting to your global config, or remove it"
        )),
        (false, None) => Ok(Some(reachable_from_anywhere(repo, &fetch_url))),
    }
}

/// `url` as git reaches it from `repo`, made to mean the same from any
/// directory: a relative path joined onto the repository, anything else as
/// it is.
fn reachable_from_anywhere(repo: &Path, url: &str) -> String {
    // `host:path` is ssh's scp-like form — and a `scheme://` URL has a colon
    // before any slash too — unless a slash comes first, making it a path.
    let names_a_host = url
        .split_once(':')
        .is_some_and(|(before, _)| !before.contains('/'));
    match names_a_host || Path::new(url).is_absolute() {
        true => url.to_string(),
        false => repo.join(url).to_string_lossy().into_owned(),
    }
}

/// Fetch `git_ref` from `remote` and return the commit it names *there*.
///
/// Writes the fetched objects and `FETCH_HEAD` into the repository's `.git`,
/// never its working tree.
async fn fetched_sha(
    repo: impl AsRef<Path>,
    remote: &str,
    git_ref: &str,
) -> anyhow::Result<String> {
    let repo = repo.as_ref();
    run_expecting_success(
        repo,
        &["fetch", "--quiet", remote, git_ref],
        &format!("fetch {remote} {git_ref}"),
    )
    .await?;
    run_expecting_success(
        repo,
        &["rev-parse", "FETCH_HEAD^{commit}"],
        "rev-parse FETCH_HEAD",
    )
    .await
}

/// Whether `remote` answered and does not carry `git_ref` — as opposed to not
/// answering at all, which is `false`: nothing is known to be missing.
pub async fn remote_lacks_ref(
    repo: impl AsRef<Path>,
    remote: &str,
    git_ref: &str,
) -> anyhow::Result<bool> {
    // `--exit-code` makes "no matching ref" exit 2, distinct from the 128 of
    // a remote that could not be read.
    let listed = run_allowing_failure(repo, &["ls-remote", "--exit-code", remote, git_ref]).await?;
    Ok(listed.exit_code == 2)
}

/// `git_ref` as `remote` has it, pinned to one commit.
///
/// # Errors
///
/// Beyond the usual, an error naming the ref when the remote does not carry
/// it — a job can only start from what a clone of the remote can see. Any
/// other fetch failure keeps git's own complaint, since pushing would not
/// fix it.
pub async fn pinned(
    repo: impl AsRef<Path>,
    remote: &str,
    git_ref: &str,
) -> anyhow::Result<PinnedRef> {
    let repo = repo.as_ref();
    match fetched_sha(repo, remote, git_ref).await {
        Ok(sha) => Ok(PinnedRef {
            name: git_ref.to_string(),
            sha,
        }),
        Err(fetch_failure) if remote_lacks_ref(repo, remote, git_ref).await? => {
            Err(anyhow::anyhow!(
                "'{git_ref}' is not on '{remote}' — a job starts from a clone of the remote, \
                 so push it first: {fetch_failure}"
            ))
        }
        Err(fetch_failure) => Err(fetch_failure),
    }
}

/// Clone `url` into the existing, empty directory `into`, without checking
/// anything out — [`check_out_new_branch`] decides what the tree holds.
pub async fn clone_into(url: &str, into: impl AsRef<Path>) -> anyhow::Result<()> {
    run_expecting_success(
        into,
        &["clone", "--quiet", "--no-checkout", url, "."],
        "clone",
    )
    .await
    .map(|_| ())
}

/// Create `branch` at `at` and check it out.
pub async fn check_out_new_branch(
    clone: impl AsRef<Path>,
    branch: &str,
    at: &str,
) -> anyhow::Result<()> {
    run_expecting_success(
        clone,
        &["checkout", "--quiet", "-b", branch, at],
        &format!("checkout -b {branch}"),
    )
    .await
    .map(|_| ())
}

/// Make every commit in a clone — the round's own, and any the agent makes —
/// carry assembly-line's identity rather than a person's, whatever config
/// the environment would otherwise supply.
pub async fn commit_as_assembly_line(clone: impl AsRef<Path>) -> anyhow::Result<()> {
    let clone = clone.as_ref();
    run_expecting_success(clone, &["config", "user.name", "assembly-line"], "config").await?;
    run_expecting_success(
        clone,
        &["config", "user.email", "assembly-line@localhost"],
        "config",
    )
    .await
    .map(|_| ())
}

/// The checked-out branch, or `None` when HEAD is detached.
pub async fn current_branch(repo: impl AsRef<Path>) -> anyhow::Result<Option<String>> {
    let name =
        run_expecting_success(repo, &["branch", "--show-current"], "branch --show-current").await?;
    Ok((!name.is_empty()).then_some(name))
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

/// Stage everything and commit. `None` means the tree was clean — a normal
/// outcome, since an agent may correctly conclude no change is needed.
pub async fn commit_all(clone: impl AsRef<Path>, message: &str) -> anyhow::Result<Option<String>> {
    commit_all_except(clone, message, &[]).await
}

/// Commit everything except `never_commit`, which stay on disk for the agent
/// to read but are kept out of history.
///
/// Staging is controlled directly rather than through `info/exclude` or a
/// `.gitignore`: both are files in the scratch clone, which the agent is free
/// to rewrite, so an ignore rule there is only as good as the agent's
/// restraint. Unstaging each path — and checking afterwards that none was
/// committed — depends on nothing the agent can edit.
///
/// # Errors
///
/// Beyond the usual git failures, an error if a `never_commit` path is
/// tracked once the commit is made — meaning the agent committed it itself.
pub async fn commit_all_except(
    clone: impl AsRef<Path>,
    message: &str,
    never_commit: &[String],
) -> anyhow::Result<Option<String>> {
    let clone = clone.as_ref();
    run_expecting_success(clone, &["add", "-A"], "add -A").await?;

    // `run_allowing_failure`: unstaging a path that was never staged is a
    // no-op worth ignoring, not an error.
    for path in never_commit {
        run_allowing_failure(clone, &["reset", "--quiet", "--", path]).await?;
    }

    let nothing_staged = run_allowing_failure(clone, &["diff", "--cached", "--quiet"])
        .await?
        .succeeded();
    if nothing_staged {
        return Ok(None);
    }

    run_expecting_success(clone, &["commit", "--no-verify", "-m", message], "commit").await?;
    ensure_untracked(clone, never_commit).await?;
    head_sha(clone).await.map(Some)
}

/// Unstaging covers the commits assembly-line makes; this catches the agent
/// committing a seeded file itself. Fatal, because a failed job is far better
/// than a leaked credential on a branch bound for a remote.
async fn ensure_untracked(clone: &Path, never_commit: &[String]) -> anyhow::Result<()> {
    if never_commit.is_empty() {
        return Ok(());
    }

    let args: Vec<&str> = ["ls-files", "--"]
        .into_iter()
        .chain(never_commit.iter().map(String::as_str))
        .collect();
    let tracked = run_expecting_success(clone, &args, "ls-files").await?;

    match tracked.is_empty() {
        true => Ok(()),
        false => Err(anyhow::anyhow!(
            "refusing to continue: seeded file(s) were committed by the agent: {}",
            tracked.lines().collect::<Vec<_>>().join(", ")
        )),
    }
}

/// Whether a clone has uncommitted changes, tracked or otherwise.
pub async fn is_dirty(clone: impl AsRef<Path>) -> anyhow::Result<bool> {
    let status = run_expecting_success(clone, &["status", "--porcelain"], "status").await?;
    Ok(!status.is_empty())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DiffStat {
    pub files: usize,
    pub insertions: usize,
    pub deletions: usize,
}

pub async fn diff_stat_against(clone: impl AsRef<Path>, base: &str) -> anyhow::Result<DiffStat> {
    let numstat = run_expecting_success(
        clone,
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
