//! One job's sandbox: a git worktree, optionally seeded with files the
//! repository does not carry.

use crate::git;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct JobWorkspace {
    pub path: PathBuf,
    pub branch: String,
    /// Relative paths copied in, which must never reach a commit.
    pub seeded: Vec<String>,
}

/// The remote a job publishes its branch to unless configured otherwise.
pub const DEFAULT_REMOTE: &str = "origin";

/// A job's branch name. Git refs are paths, so this must never nest under
/// another ref assembly-line creates.
#[must_use]
pub fn job_branch_name(job_id: u64) -> String {
    format!("al/job-{job_id}")
}

/// # Errors
///
/// Seed paths are checked *before* the worktree is made, so a typo leaves
/// nothing behind.
pub async fn create(
    repo: impl AsRef<Path>,
    path: impl AsRef<Path>,
    branch: &str,
    start: &git::WorktreeStart,
    seed_from: impl AsRef<Path>,
    copy_paths: &[String],
) -> anyhow::Result<JobWorkspace> {
    let (repo, path, seed_from) = (repo.as_ref(), path.as_ref(), seed_from.as_ref());

    if let Some(missing) = missing_seed_path(seed_from, copy_paths) {
        anyhow::bail!(
            "copy path '{missing}' does not exist under {}",
            seed_from.display()
        );
    }

    check_out_branch(repo, path, branch, start).await?;
    seed_files(path, seed_from, copy_paths)?;

    Ok(JobWorkspace {
        path: path.to_path_buf(),
        branch: branch.to_string(),
        seeded: copy_paths.to_vec(),
    })
}

/// The first `copy` path the repository declares that is not actually there.
fn missing_seed_path<'a>(seed_from: &Path, copy_paths: &'a [String]) -> Option<&'a String> {
    copy_paths.iter().find(|rel| !seed_from.join(rel).exists())
}

/// Put a worktree at `path` holding `branch`, freeing whatever held the path —
/// and, for a fresh branch, the branch name — first.
async fn check_out_branch(
    repo: &Path,
    path: &Path,
    branch: &str,
    start: &git::WorktreeStart,
) -> anyhow::Result<()> {
    match start {
        git::WorktreeStart::CreatingBranch { .. } => {
            clear_previous_attempt(repo, path, branch).await?;
        }
        git::WorktreeStart::OnExistingBranch { .. } => {
            clear_previous_checkout(repo, path).await?;
        }
    }

    git::add_worktree(repo, path, branch, start).await
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

/// Free the checkout path, whatever is holding it. Anything found here is the
/// residue of a job that died mid-round, since jobs discard their own scratch.
async fn clear_previous_checkout(repo: &Path, path: &Path) -> anyhow::Result<()> {
    git::prune_worktrees(repo).await?;

    if path.exists() {
        git::remove_worktree(repo, path).await?;
    }
    Ok(())
}

/// Free both the checkout path and the branch name, so a fresh attempt can
/// take them.
async fn clear_previous_attempt(repo: &Path, path: &Path, branch: &str) -> anyhow::Result<()> {
    clear_previous_checkout(repo, path).await?;

    match git::branch_exists(repo, branch).await? {
        true => git::delete_branch(repo, branch).await,
        false => Ok(()),
    }
}

/// `None` means the agent changed nothing.
///
/// # Errors
///
/// See [`git::commit_all_except`].
pub async fn commit(ws: &JobWorkspace, message: &str) -> anyhow::Result<Option<String>> {
    git::commit_all_except(&ws.path, message, &ws.seeded).await
}

/// Make the job's branch durable, returning the remote it reached, or `None`
/// when the repository has no such remote — see
/// [`EventKind::JobBranchPublished`](crate::event::EventKind::JobBranchPublished).
///
/// # Errors
///
/// Returns an error if the remote cannot be listed, or if the push is
/// rejected. Note the asymmetry: a *missing* remote is `Ok(None)`, a
/// *refused* push is `Err` — callers that treat the two alike flatten it
/// themselves.
pub async fn publish(
    repo: impl AsRef<Path>,
    ws: &JobWorkspace,
    remote: &str,
) -> anyhow::Result<Option<String>> {
    let repo = repo.as_ref();
    match git::remote_exists(repo, remote).await? {
        false => Ok(None),
        true => git::push_branch(repo, remote, &ws.branch)
            .await
            .map(|()| Some(remote.to_string())),
    }
}

/// Remove the checkout. The branch survives.
///
/// # Errors
///
/// See [`git::remove_worktree`].
pub async fn discard(repo: impl AsRef<Path>, ws: &JobWorkspace) -> anyhow::Result<()> {
    git::remove_worktree(repo, &ws.path).await
}
