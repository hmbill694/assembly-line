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
///
/// `git` leads a process group of its own, detached from any terminal so a
/// credential or host-key prompt fails rather than waits forever (see
/// `exec::detach_from_terminal`). One dropped before it finishes —
/// a clone whose caller gave up waiting — is killed with that whole group,
/// so an `ssh` or remote helper it started does not outlive it.
pub async fn run_allowing_failure(
    dir: impl AsRef<Path>,
    args: &[&str],
) -> anyhow::Result<GitOutput> {
    let failed = |e: std::io::Error| anyhow::anyhow!("running `git {}`: {e}", args.join(" "));
    let child = crate::exec::detach_from_terminal(&mut Command::new("git"))
        .args(args)
        // Machine-readable formats (`--porcelain`, `--numstat`) are stable, but
        // pinning the locale keeps any incidental output predictable too.
        .env("LC_ALL", "C")
        .current_dir(dir.as_ref())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(failed)?;
    let group = GroupKilledOnDrop(child.id().and_then(|id| i32::try_from(id).ok()));
    let output = child.wait_with_output().await.map_err(failed)?;
    group.disarm();

    Ok(GitOutput {
        exit_code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// A process group killed when this is dropped, unless disarmed first.
struct GroupKilledOnDrop(Option<i32>);

impl GroupKilledOnDrop {
    /// The leader finished on its own; nothing is left to kill.
    fn disarm(mut self) {
        self.0 = None;
    }
}

impl Drop for GroupKilledOnDrop {
    fn drop(&mut self) {
        if let Some(group) = self.0 {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(group),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
    }
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

/// Whether `repo` has `sha` as a commit.
pub async fn has_commit(repo: impl AsRef<Path>, sha: &str) -> anyhow::Result<bool> {
    let spec = format!("{sha}^{{commit}}");
    Ok(run_allowing_failure(repo, &["cat-file", "-e", &spec])
        .await?
        .succeeded())
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
    let names_a_host = url.contains("://") || scp_like_parts(url).is_some();
    match names_a_host || Path::new(url).is_absolute() {
        true => url.to_string(),
        false => repo.join(url).to_string_lossy().into_owned(),
    }
}

/// git's scp-like `[user@]host:path`, split at the colon that ends the host,
/// or `None` for anything else. git recognises it by a colon before any
/// slash; a URL with a scheme is never one, and a local path has no such
/// colon.
pub(crate) fn scp_like_parts(url: &str) -> Option<(&str, &str)> {
    (!url.contains("://"))
        .then(|| split_at_host_colon(url))
        .flatten()
        .filter(|(authority, _)| !authority.contains('/'))
}

/// `s` split at the colon that ends a host: the first one outside a
/// bracketed IPv6 literal, whose own colons end nothing.
pub(crate) fn split_at_host_colon(s: &str) -> Option<(&str, &str)> {
    let searched_from = match (s.find('['), s.find(':')) {
        (Some(open), Some(colon)) if open < colon => {
            s[open..].find(']').map_or(open, |close| open + close)
        }
        _ => 0,
    };
    let colon = searched_from + s[searched_from..].find(':')?;
    Some((&s[..colon], &s[colon + 1..]))
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

/// The branches `remote` carries whose names match `pattern` — a glob such
/// as `al/job-*` — without their `refs/heads/` prefix.
pub async fn remote_branches_matching(
    repo: impl AsRef<Path>,
    remote: &str,
    pattern: &str,
) -> anyhow::Result<Vec<String>> {
    let listed = run_expecting_success(
        repo,
        &[
            "ls-remote",
            "--heads",
            remote,
            &format!("refs/heads/{pattern}"),
        ],
        &format!("ls-remote {remote}"),
    )
    .await?;
    // "<sha>\trefs/heads/<branch>" per line.
    Ok(listed
        .lines()
        .filter_map(|line| line.split_once("\trefs/heads/"))
        .map(|(_, branch)| branch.to_string())
        .collect())
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

/// A credential helper answering with the token in
/// [`crate::payload::GIT_TOKEN_VAR`], read from the environment when git
/// asks — never written to disk.
pub const TOKEN_CREDENTIAL_HELPER: &str = "!f() { test \"$1\" = get && echo username=x-access-token && echo \"password=$ASSEMBLY_GIT_TOKEN\"; }; f";

/// Clone `url` into the existing, empty directory `into`, without checking
/// anything out — [`check_out_new_branch`] decides what the tree holds.
///
/// A `credential_helper` authenticates the clone and stays configured in it,
/// so the job's push authenticates the same way. It *replaces* any helper
/// the system or global config names: an empty `credential.helper` resets
/// the list, so git neither asks another helper first nor hands one the
/// token to `store` once it has worked. `clone -c` writes both settings into
/// the new repository before anything is fetched, so one command covers the
/// clone and every later push.
pub async fn clone_into(
    url: &str,
    into: impl AsRef<Path>,
    credential_helper: Option<&str>,
) -> anyhow::Result<()> {
    let helper_setting = credential_helper.map(|helper| format!("credential.helper={helper}"));
    let args: Vec<&str> = ["clone", "--quiet", "--no-checkout"]
        .into_iter()
        .chain(
            helper_setting
                .iter()
                .flat_map(|setting| ["-c", "credential.helper=", "-c", setting.as_str()]),
        )
        .chain([url, "."])
        .collect();
    run_expecting_success(into, &args, "clone")
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

/// Publish whatever the clone has checked out as `branch` on `remote`.
///
/// `HEAD`, not the local `branch`: the agent is free to check out another
/// branch, and what gets pushed must be the commit everything before the push
/// inspected — the recorded sha reads `HEAD`.
///
/// Deliberately without `--set-upstream`: that would write `branch.*.remote`
/// into the repository's config. A later round pushes the same branch name
/// again and fast-forwards without it.
///
/// Runs no hooks: a round's clone is the agent's to write, `.git/hooks` and
/// `core.hooksPath` included, and the push runs with the git token in its
/// environment. A `-c` on the command line outranks anything the clone's
/// own config says. This is the push's guarantee, not the round's: the
/// `commit --no-verify` before it still runs `prepare-commit-msg`,
/// `post-commit` and `reference-transaction`, and every git command in the
/// clone reads its config — which matters only as much as the token is
/// hidden from the agent, and the spec's accepted risks say it is not.
pub async fn push_head_as(
    repo: impl AsRef<Path>,
    remote: &str,
    branch: &str,
) -> anyhow::Result<()> {
    let refspec = format!("HEAD:refs/heads/{branch}");
    run_expecting_success(
        repo,
        &["-c", "core.hooksPath=/dev/null", "push", remote, &refspec],
        &format!("push {remote} {branch}"),
    )
    .await
    .map(|_| ())
}

/// Create `branch` on `remote` at `sha`, only if `remote` has no such branch.
/// `false` means somebody already has it.
///
/// Neither the exit code nor the lease decides this alone. A push of the
/// commit a branch already points at exits 0 as "up to date", lease or not,
/// so only the porcelain line's `*` (a new branch) counts as created. The
/// lease — `--force-with-lease=<ref>:`, expecting no such ref — stops the
/// push fast-forwarding a branch that sits at an ancestor of `sha`. And a
/// push that lost its race on the remote itself, after both sides saw the
/// ref absent, reports "reference already exists" rather than a lost lease.
/// No hooks run, as for [`push_head_as`].
pub async fn create_branch_if_absent(
    repo: impl AsRef<Path>,
    remote: &str,
    sha: &str,
    branch: &str,
) -> anyhow::Result<bool> {
    let target = format!("refs/heads/{branch}");
    let lease = format!("--force-with-lease={target}:");
    let refspec = format!("{sha}:{target}");
    let pushed = run_allowing_failure(
        repo,
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "push",
            "--porcelain",
            &lease,
            remote,
            &refspec,
        ],
    )
    .await?;
    // "<flag>\t<from>:<to>\t<summary>" per ref.
    let flag_and_summary = pushed.stdout.lines().find_map(|line| {
        let mut fields = line.split('\t');
        let (flag, refs, summary) = (fields.next()?, fields.next()?, fields.next()?);
        refs.strip_suffix(target.as_str())?
            .ends_with(':')
            .then_some((flag, summary))
    });
    match flag_and_summary {
        Some(("*", _)) => Ok(true),
        Some(("=", _)) => Ok(false),
        Some(("!", summary))
            if summary.starts_with("[rejected]")
                || summary.ends_with("(reference already exists)") =>
        {
            Ok(false)
        }
        refused => Err(anyhow::anyhow!(
            "creating {branch} on '{remote}' failed (exit {}): {}",
            pushed.exit_code,
            [
                refused.map_or("", |(_, summary)| summary),
                pushed.stderr.trim()
            ]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
        )),
    }
}

/// Stage everything in `clone` and commit it, returning the new commit, or
/// `None` when there was nothing to commit.
pub async fn commit_all(clone: impl AsRef<Path>, message: &str) -> anyhow::Result<Option<String>> {
    let clone = clone.as_ref();
    run_expecting_success(clone, &["add", "-A"], "add -A").await?;

    let nothing_staged = run_allowing_failure(clone, &["diff", "--cached", "--quiet"])
        .await?
        .succeeded();
    if nothing_staged {
        return Ok(None);
    }
    run_expecting_success(clone, &["commit", "--no-verify", "-m", message], "commit").await?;
    head_sha(clone).await.map(Some)
}

/// Whether `HEAD` carries any commit that `since` does not — work the round
/// made, whoever committed it.
pub async fn head_is_ahead_of(repo: impl AsRef<Path>, since: &str) -> anyhow::Result<bool> {
    commit_is_ahead_of(repo, "HEAD", since).await
}

/// Whether `commit` carries any commit that `since` does not.
pub async fn commit_is_ahead_of(
    repo: impl AsRef<Path>,
    commit: &str,
    since: &str,
) -> anyhow::Result<bool> {
    let range = format!("{since}..{commit}");
    let count = run_expecting_success(repo, &["rev-list", "--count", &range], "rev-list").await?;
    Ok(count != "0")
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
