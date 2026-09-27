//! One job's sandbox: a scratch clone of the remote, optionally seeded with
//! files the repository does not carry.

use crate::git::{self, PinnedRef};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

#[derive(Debug)]
pub struct JobWorkspace {
    /// Deleted when the workspace is dropped, which is what makes the
    /// checkout scratch whatever becomes of the round.
    dir: tempfile::TempDir,
    pub branch: String,
    /// Relative paths copied in, which must never reach a commit.
    seeded: Vec<String>,
    /// The commit the branch was checked out at — every commit since is
    /// this round's.
    started_at: String,
}

impl JobWorkspace {
    #[must_use]
    pub fn path(&self) -> &Path {
        self.dir.path()
    }
}

/// The repository's remote a job clones from and publishes its branch to.
pub const DEFAULT_REMOTE: &str = "origin";

/// What `git clone` names the remote a scratch clone came from — whatever
/// the repository itself calls that remote.
const CLONE_REMOTE: &str = "origin";

/// A job's branch name. Git refs are paths, so this must never nest under
/// another ref assembly-line creates.
#[must_use]
pub fn job_branch_name(job_id: u64) -> String {
    format!("al/job-{job_id}")
}

/// Matches every name [`job_branch_name`] makes, for asking a remote which
/// jobs it already carries.
pub const JOB_BRANCH_PATTERN: &str = "al/job-*";

/// The job a branch name was made for by [`job_branch_name`], or `None` for
/// any branch that is not a job's — including near misses like `al/job-007`,
/// which that function never makes.
#[must_use]
pub fn job_id_from_branch_name(branch: &str) -> Option<u64> {
    let id = branch.strip_prefix("al/job-")?.parse().ok()?;
    (job_branch_name(id) == branch).then_some(id)
}

/// Clone `remote_url` into a fresh directory under `scratch_root`, with
/// `branch` checked out at `start`, and seed it. A `credential_helper`
/// authenticates both the clone and the eventual push.
///
/// # Errors
///
/// Seed paths are checked *before* anything is cloned, so a typo leaves
/// nothing behind. A clone that fails midway leaves nothing either: the
/// directory is removed as the error propagates.
pub async fn create(
    remote_url: &str,
    start: &PinnedRef,
    branch: &str,
    seed_from: impl AsRef<Path>,
    copy_paths: &[String],
    scratch_root: impl AsRef<Path>,
    credential_helper: Option<&str>,
) -> anyhow::Result<JobWorkspace> {
    let seed_from = seed_from.as_ref();

    if let Some(missing) = missing_seed_path(seed_from, copy_paths) {
        anyhow::bail!(
            "copy path '{missing}' does not exist under {}",
            seed_from.display()
        );
    }

    std::fs::create_dir_all(scratch_root.as_ref())?;
    let dir = tempfile::Builder::new()
        .prefix("assembly-job-")
        .tempdir_in(scratch_root)?;

    git::clone_into(remote_url, dir.path(), credential_helper).await?;
    // A tag, or a commit reachable only from the ref the job names, is not
    // guaranteed by a plain clone.
    git::run_allowing_failure(dir.path(), &["fetch", "--quiet", CLONE_REMOTE, &start.name]).await?;
    git::check_out_new_branch(dir.path(), branch, &start.sha).await?;
    git::commit_as_assembly_line(dir.path()).await?;
    seed_files(dir.path(), seed_from, copy_paths)?;

    Ok(JobWorkspace {
        dir,
        branch: branch.to_string(),
        seeded: copy_paths.to_vec(),
        started_at: start.sha.clone(),
    })
}

/// The first `copy` path the repository declares that is not actually there.
fn missing_seed_path<'a>(seed_from: &Path, copy_paths: &'a [String]) -> Option<&'a String> {
    copy_paths.iter().find(|rel| !seed_from.join(rel).exists())
}

/// Copy the repository's `copy` paths into the checkout, creating whatever
/// directories they nest in.
fn seed_files(into: &Path, seed_from: &Path, copy_paths: &[String]) -> anyhow::Result<()> {
    copy_paths.iter().try_for_each(|rel| -> anyhow::Result<()> {
        let destination = into.join(rel);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(seed_from.join(rel), destination)
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!("copying '{rel}' into the workspace: {e}"))
    })
}

/// Commit what the agent left uncommitted, and return the commit the round
/// ends on. `None` means the round made nothing: the agent changed nothing
/// and committed nothing. An agent that committed its own work and left the
/// tree clean still has work to publish.
///
/// # Errors
///
/// See [`git::commit_all_except`].
pub async fn commit(ws: &JobWorkspace, message: &str) -> anyhow::Result<Option<String>> {
    git::commit_all_except(ws.path(), message, &ws.seeded, &ws.started_at).await?;
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
pub async fn publish(ws: &JobWorkspace) -> anyhow::Result<()> {
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
pub fn discard(ws: JobWorkspace) -> std::io::Result<()> {
    restore_owner_permissions(ws.path())?;
    ws.dir.close()
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
