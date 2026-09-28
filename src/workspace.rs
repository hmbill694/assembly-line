//! One round's sandbox: a scratch clone of the remote.

use crate::git::{self, PinnedRef};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

/// A scratch clone of the remote, with nothing checked out yet.
#[derive(Debug)]
pub struct ScratchClone {
    /// Deleted when the clone is dropped, which is what makes the checkout
    /// scratch whatever becomes of the round.
    dir: tempfile::TempDir,
}

impl ScratchClone {
    #[must_use]
    pub fn path(&self) -> &Path {
        self.dir.path()
    }
}

#[derive(Debug)]
pub struct RoundWorkspace {
    scratch: ScratchClone,
    pub branch: String,
    /// The commit the branch was checked out at — every commit since is
    /// this round's.
    started_at: String,
}

impl RoundWorkspace {
    #[must_use]
    pub fn path(&self) -> &Path {
        self.scratch.path()
    }
}

/// The repository's remote a job clones from and publishes its branch to.
pub const DEFAULT_REMOTE: &str = "origin";

/// What `git clone` names the remote a scratch clone came from — whatever
/// the repository itself calls that remote.
const CLONE_REMOTE: &str = "origin";

/// Clone `remote_url` into a fresh directory under `scratch_root`, with
/// `branch` checked out at `start`. A `credential_helper` authenticates both
/// the clone and the eventual push.
///
/// # Errors
///
/// A clone that fails midway leaves nothing: the directory is removed as
/// the error propagates.
pub async fn create(
    remote_url: &str,
    start: &PinnedRef,
    branch: &str,
    scratch_root: impl AsRef<Path>,
    credential_helper: Option<&str>,
) -> anyhow::Result<RoundWorkspace> {
    let clone = clone_scratch(remote_url, scratch_root, credential_helper).await?;
    // A tag, or a commit reachable only from the ref the job names, is not
    // guaranteed by a plain clone.
    git::run_allowing_failure(
        clone.path(),
        &["fetch", "--quiet", CLONE_REMOTE, &start.name],
    )
    .await?;
    start_round(clone, start, branch).await
}

/// Clone `remote_url` into a fresh directory under `scratch_root`, checking
/// nothing out. A `credential_helper` authenticates both the clone and every
/// later fetch and push from it.
///
/// # Errors
///
/// A clone that fails midway leaves nothing: the directory is removed as
/// the error propagates.
pub async fn clone_scratch(
    remote_url: &str,
    scratch_root: impl AsRef<Path>,
    credential_helper: Option<&str>,
) -> anyhow::Result<ScratchClone> {
    std::fs::create_dir_all(scratch_root.as_ref())?;
    let dir = tempfile::Builder::new()
        .prefix("assembly-round-")
        .tempdir_in(scratch_root)?;
    git::clone_into(remote_url, dir.path(), credential_helper).await?;
    Ok(ScratchClone { dir })
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
    pinned_sha: Option<&str>,
) -> anyhow::Result<PinnedRef> {
    let tip = git::pinned(clone.path(), CLONE_REMOTE, git_ref).await?;
    let Some(sha) = pinned_sha else {
        return Ok(tip);
    };
    // A ref that moved on past the pin still has it in its history; one that
    // was rewritten may not, so ask the remote for the commit itself.
    if !git::has_commit(clone.path(), sha).await? {
        git::run_allowing_failure(clone.path(), &["fetch", "--quiet", CLONE_REMOTE, sha]).await?;
    }
    match git::has_commit(clone.path(), sha).await? {
        true => Ok(PinnedRef {
            name: git_ref.to_string(),
            sha: sha.to_string(),
        }),
        false => Err(anyhow::anyhow!(
            "'{sha}' is not a commit '{CLONE_REMOTE}' can supply for '{git_ref}'"
        )),
    }
}

/// The tip of job branch `branch` on the clone's remote, or `None` when the
/// remote has no such branch.
///
/// # Errors
///
/// When the remote cannot be asked.
pub async fn job_branch_in_clone(
    clone: &ScratchClone,
    branch: &str,
) -> anyhow::Result<Option<PinnedRef>> {
    match git::remote_lacks_ref(clone.path(), CLONE_REMOTE, branch).await? {
        true => Ok(None),
        false => git::pinned(clone.path(), CLONE_REMOTE, branch)
            .await
            .map(Some),
    }
}

/// Check `branch` out at `start` in `clone`, committing as assembly-line,
/// and make it the round's workspace.
///
/// # Errors
///
/// When the checkout or the identity cannot be set.
pub async fn start_round(
    clone: ScratchClone,
    start: &PinnedRef,
    branch: &str,
) -> anyhow::Result<RoundWorkspace> {
    git::check_out_new_branch(clone.path(), branch, &start.sha).await?;
    git::commit_as_assembly_line(clone.path()).await?;
    Ok(RoundWorkspace {
        scratch: clone,
        branch: branch.to_string(),
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
pub async fn commit(ws: &RoundWorkspace, message: &str) -> anyhow::Result<Option<String>> {
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

/// Push the commit the clone ends on to the clone's `origin`, as the job's
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
    let mut last_failure = None;
    for wait in PUSH_BACKOFF {
        tokio::time::sleep(wait).await;
        match git::push_head_as(ws.path(), CLONE_REMOTE, &ws.branch).await {
            Ok(()) => return Ok(()),
            Err(e) => last_failure = Some(e),
        }
    }

    Err(anyhow::anyhow!(
        "could not publish {} — the work is lost with the scratch checkout: {}",
        ws.branch,
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
