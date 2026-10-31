//! One round's sandbox: a scratch clone of the remote.

use crate::admission::Admitted;
use crate::claim::claim_job;
use crate::git::{self, CommitId, PinnedRef};
use crate::job::JobId;
use crate::payload::GIT_TOKEN_VAR;
use crate::remote::Remote;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// How long cloning and provisioning may take together. Not `max_duration`:
/// that is the repository's statement about its own commands, and a cold
/// toolchain install should not eat the agent's budget.
pub const PROVISIONING_LIMIT: Duration = Duration::from_mins(15);

/// A scratch clone of the remote, with nothing checked out yet.
#[derive(Debug)]
pub struct ScratchClone {
    /// Deleted when the clone is dropped, which is what makes the checkout
    /// scratch whatever becomes of the round.
    dir: tempfile::TempDir,
    remote: Remote,
}

impl ScratchClone {
    #[must_use]
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Where the clone came from, which every fetch and push from it reaches
    /// by this URL rather than by the name `git clone` gave it, so what is
    /// said about the remote names its URL.
    #[must_use]
    pub fn remote(&self) -> &Remote {
        &self.remote
    }
}

#[derive(Debug)]
pub struct RoundWorkspace {
    scratch: ScratchClone,
    job: JobId,
    /// The commit the branch was checked out at — every commit since is
    /// this round's.
    started_at: CommitId,
}

impl RoundWorkspace {
    #[must_use]
    pub fn path(&self) -> &Path {
        self.scratch.path()
    }

    /// Where [`publish`] pushes the branch.
    #[must_use]
    pub fn remote(&self) -> &Remote {
        self.scratch.remote()
    }
}

/// Clone `remote` into a fresh directory under `scratch_root`, checking
/// nothing out. A `credential_helper` authenticates both the clone and every
/// later fetch and push from it.
///
/// # Errors
///
/// A clone that fails midway leaves nothing: the directory is removed as
/// the error propagates.
pub async fn clone_scratch(
    remote: &Remote,
    scratch_root: impl AsRef<Path>,
    credential_helper: Option<&str>,
) -> anyhow::Result<ScratchClone> {
    std::fs::create_dir_all(scratch_root.as_ref())?;
    let dir = tempfile::Builder::new()
        .prefix("assembly-round-")
        .tempdir_in(scratch_root)?;
    git::clone_into(remote.url(), dir.path(), credential_helper).await?;
    Ok(ScratchClone {
        dir,
        remote: remote.clone(),
    })
}

/// [`clone_scratch`] with this environment's credentials, cut off at
/// [`PROVISIONING_LIMIT`] or by `cancel`. The deadline that comes back with
/// the clone is that limit, counted from before the clone began: what is
/// left of it is provisioning's.
///
/// # Errors
///
/// The clone's own error, or one saying it timed out or was cancelled.
pub async fn clone_within_provisioning_limit(
    remote: &Remote,
    scratch_root: impl AsRef<Path>,
    cancel: &CancellationToken,
) -> anyhow::Result<(ScratchClone, Instant)> {
    let provisioning_deadline = Instant::now() + PROVISIONING_LIMIT;
    let clone = within_provisioning_limit(
        clone_scratch(
            remote,
            scratch_root,
            credential_helper_for_this_environment(),
        ),
        cancel,
    )
    .await?;
    Ok((clone, provisioning_deadline))
}

/// `cloning`, cut off at [`PROVISIONING_LIMIT`] or by `cancel`.
///
/// Unlike `mise`, a clone may be dropped from outside, because dropping
/// `git` kills its whole process group (`git::run_allowing_failure`).
async fn within_provisioning_limit<T>(
    cloning: impl Future<Output = anyhow::Result<T>>,
    cancel: &CancellationToken,
) -> anyhow::Result<T> {
    tokio::select! {
        cloned = tokio::time::timeout(PROVISIONING_LIMIT, cloning) => cloned.map_err(|_| {
            anyhow::anyhow!(
                "provisioning timed out after {}",
                humantime::format_duration(PROVISIONING_LIMIT)
            )
        })?,
        () = cancel.cancelled() => Err(anyhow::anyhow!("cancelled")),
    }
}

/// The credential helper a job's clone authenticates with. A container
/// runner always sends the token, having no other credentials to offer;
/// without one, git uses whatever this environment already has.
fn credential_helper_for_this_environment() -> Option<&'static str> {
    std::env::var_os(GIT_TOKEN_VAR)
        .is_some()
        .then_some(git::TOKEN_CREDENTIAL_HELPER)
}

/// `git_ref` as the clone's remote has it, pinned to `pinned_sha` when one
/// is given — a commit validated earlier, which the ref may have moved past
/// since.
///
/// # Errors
///
/// When the remote does not carry `git_ref`, or `pinned_sha` is not a
/// commit the remote can supply.
pub async fn pin_in_clone(
    clone: &ScratchClone,
    git_ref: &str,
    pinned_sha: Option<&CommitId>,
) -> anyhow::Result<PinnedRef> {
    let url = clone.remote().url();
    let tip = git::pinned(clone.path(), url, git_ref).await?;
    let Some(sha) = pinned_sha else {
        return Ok(tip);
    };
    // A ref that moved on past the pin still has it in its history; one that
    // was rewritten may not, so ask the remote for the commit itself.
    if !git::has_commit(clone.path(), sha).await? {
        git::run_allowing_failure(clone.path(), &["fetch", "--quiet", url, sha.as_str()]).await?;
    }
    match git::has_commit(clone.path(), sha).await? {
        true => Ok(PinnedRef {
            name: git_ref.to_string(),
            sha: sha.clone(),
        }),
        false => Err(anyhow::anyhow!(
            "'{sha}' is not a commit {url} can supply for '{git_ref}'"
        )),
    }
}

/// The tip of job `id`'s branch on the clone's remote, or `None` when the
/// remote has no such branch.
///
/// # Errors
///
/// When the remote cannot be asked.
pub async fn job_branch_in_clone(
    clone: &ScratchClone,
    id: JobId,
) -> anyhow::Result<Option<PinnedRef>> {
    let (url, branch) = (clone.remote().url(), id.branch_name());
    match git::remote_lacks_ref(clone.path(), url, &branch).await? {
        true => Ok(None),
        false => git::pinned(clone.path(), url, &branch).await.map(Some),
    }
}

/// The next free job id on the clone's remote, claimed at the admitted base.
///
/// # Errors
///
/// See [`claim_job`].
pub async fn claim_job_in_clone(
    clone: &ScratchClone,
    admitted: &Admitted,
) -> anyhow::Result<JobId> {
    claim_job(clone.path(), clone.remote().url(), admitted).await
}

/// Check job `job`'s branch out at `start` in `clone`, committing as
/// assembly-line, and make it the round's workspace.
///
/// # Errors
///
/// When the checkout or the identity cannot be set.
pub async fn start_round(
    clone: ScratchClone,
    start: &PinnedRef,
    job: JobId,
) -> anyhow::Result<RoundWorkspace> {
    git::check_out_new_branch(clone.path(), &job.branch_name(), &start.sha).await?;
    git::commit_as_assembly_line(clone.path()).await?;
    Ok(RoundWorkspace {
        scratch: clone,
        job,
        started_at: start.sha.clone(),
    })
}

/// Commit what the agent left uncommitted, and return the commit the round
/// ends on. `None` means the round made nothing: the agent changed nothing
/// and committed nothing. An agent that committed its own work and left the
/// tree clean still has work to publish.
///
/// # Errors
///
/// See [`git::commit_all`].
pub async fn commit(ws: &RoundWorkspace, message: &str) -> anyhow::Result<Option<CommitId>> {
    git::commit_all(ws.path(), message).await?;
    match git::head_is_ahead_of(ws.path(), &ws.started_at).await? {
        true => git::head_sha(ws.path()).await.map(Some),
        false => Ok(None),
    }
}

/// Waits before each push attempt. A transient network failure is worth
/// riding out; a refusal is not, but three quick attempts cost little.
const PUSH_BACKOFF: [Duration; 3] = [
    Duration::ZERO,
    Duration::from_millis(500),
    Duration::from_secs(2),
];

/// Push the commit the clone ends on to the clone's remote, as the job's
/// branch.
///
/// A loop rather than a combinator: each attempt is sequential I/O, and the
/// first success ends it.
///
/// # Errors
///
/// When every attempt fails, an error carrying git's last complaint and
/// saying what that costs: the branch exists only in this scratch clone, so
/// the work goes with it.
pub async fn publish(ws: &RoundWorkspace) -> anyhow::Result<()> {
    let branch = ws.job.branch_name();
    let mut last_failure = None;
    for wait in PUSH_BACKOFF {
        tokio::time::sleep(wait).await;
        match git::push_head_as(ws.path(), ws.remote().url(), &branch).await {
            Ok(()) => return Ok(()),
            Err(e) => last_failure = Some(e),
        }
    }

    Err(anyhow::anyhow!(
        "could not publish {branch} — the work is lost with the scratch checkout: {}",
        last_failure.map(|e| e.to_string()).unwrap_or_default()
    ))
}

/// Remove the checkout. The branch lives on the remote.
///
/// The checkout is the agent's to change, permissions included, and a
/// directory it made unwritable cannot be emptied — so every directory's
/// owner permissions are restored first.
///
/// # Errors
///
/// Returns an error if the directory cannot be removed.
pub fn discard(ws: RoundWorkspace) -> std::io::Result<()> {
    restore_owner_permissions(ws.path())?;
    ws.scratch.dir.close()
}

/// Give the owner full permission on `dir` and every directory below it,
/// without following symlinks out of it.
fn restore_owner_permissions(dir: &Path) -> std::io::Result<()> {
    let mut permissions = std::fs::symlink_metadata(dir)?.permissions();
    permissions.set_mode(permissions.mode() | 0o700);
    std::fs::set_permissions(dir, permissions)?;
    std::fs::read_dir(dir)?.try_for_each(|entry| {
        let entry = entry?;
        match entry.file_type()?.is_dir() {
            true => restore_owner_permissions(&entry.path()),
            false => Ok(()),
        }
    })
}
