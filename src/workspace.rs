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

/// Where a job's checkout begins.
#[derive(Debug, Clone, Copy)]
pub enum StartPoint<'a> {
    /// Cut a fresh branch at this commit, superseding any earlier attempt's.
    FreshBranch(&'a str),
    /// Continue the job's existing branch. A revise round is a new job, but
    /// it appends to the branch rather than replacing the record of what came
    /// before — which is also how the agent sees its own prior work, as files
    /// on disk, with no session replay.
    ContinueBranch,
}

/// Create a worktree for `branch` and seed it.
///
/// # Errors
///
/// Returns an error if a declared seed path does not exist under `seed_from`,
/// if the worktree cannot be created, or if a copy fails. Seed paths are
/// checked before the worktree is made, so a typo leaves nothing behind.
pub async fn create(
    repo: impl AsRef<Path>,
    path: impl AsRef<Path>,
    branch: &str,
    start: StartPoint<'_>,
    seed_from: impl AsRef<Path>,
    copy_paths: &[String],
) -> anyhow::Result<JobWorkspace> {
    let (repo, path, seed_from) = (repo.as_ref(), path.as_ref(), seed_from.as_ref());

    if let Some(missing) = copy_paths.iter().find(|rel| !seed_from.join(rel).exists()) {
        anyhow::bail!(
            "copy path '{missing}' does not exist under {}",
            seed_from.display()
        );
    }

    match start {
        StartPoint::FreshBranch(base_sha) => {
            clear_previous_attempt(repo, path, branch).await?;
            git::add_worktree(repo, path, branch, base_sha).await?;
        }
        StartPoint::ContinueBranch => {
            clear_previous_checkout(repo, path).await?;
            git::add_worktree_for_existing_branch(repo, path, branch).await?;
        }
    }

    copy_paths
        .iter()
        .try_for_each(|rel| -> anyhow::Result<()> {
            let destination = path.join(rel);
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(seed_from.join(rel), destination)
                .map(|_| ())
                .map_err(|e| anyhow::anyhow!("copying '{rel}' into the workspace: {e}"))
        })?;

    Ok(JobWorkspace {
        path: path.to_path_buf(),
        branch: branch.to_string(),
        seeded: copy_paths.to_vec(),
    })
}

/// Free the checkout path, whatever is holding it.
///
/// Jobs discard their own scratch, so anything found here is the residue of a
/// job that died mid-round. Git will not reuse the path while it is claimed —
/// and a worktree whose directory is already gone still holds its
/// administrative entry, which alone is enough to make the name unusable.
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

/// Commit whatever the agent left, excluding seeded files. `None` means the
/// agent changed nothing.
///
/// # Errors
///
/// See [`git::commit_all_except`] — notably, an error if the agent committed a
/// seeded file itself.
pub async fn commit(ws: &JobWorkspace, message: &str) -> anyhow::Result<Option<String>> {
    git::commit_all_except(&ws.path, message, &ws.seeded).await
}

/// Make the job's branch durable, returning the remote it reached.
///
/// A job is stateless: its checkout is scratch and the branch is the only
/// thing that outlives it. `None` means the repository has no such remote, so
/// the branch stays a local ref — an ordinary local run, not a failure.
///
/// # Errors
///
/// Returns an error if the remote cannot be listed, or if the push is
/// rejected.
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

/// Remove the checkout. The branch survives, because it is the record of what
/// the job did.
///
/// # Errors
///
/// Returns an error if git cannot remove the worktree.
pub async fn discard(repo: impl AsRef<Path>, ws: &JobWorkspace) -> anyhow::Result<()> {
    git::remove_worktree(repo, &ws.path).await
}
