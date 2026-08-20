//! One node's sandbox: a git worktree, optionally seeded with files the
//! repository does not carry.

use crate::git;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct NodeWorkspace {
    pub path: PathBuf,
    pub branch: String,
    /// Relative paths copied in, which must never reach a commit.
    pub seeded: Vec<String>,
}

#[must_use]
pub fn run_branch_name(run_id: u64) -> String {
    format!("al/run-{run_id}")
}

/// Node branches are flat siblings of the run branch. Git refs are paths, so
/// `al/run-42` and `al/run-42/node` cannot both exist.
#[must_use]
pub fn node_branch_name(run_id: u64, node: &str) -> String {
    format!("al/run-{run_id}-{node}")
}

/// Create a worktree for `branch` at `base_sha` and seed it.
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
    base_sha: &str,
    seed_from: impl AsRef<Path>,
    copy_paths: &[String],
) -> anyhow::Result<NodeWorkspace> {
    let (repo, path, seed_from) = (repo.as_ref(), path.as_ref(), seed_from.as_ref());

    if let Some(missing) = copy_paths.iter().find(|rel| !seed_from.join(rel).exists()) {
        anyhow::bail!(
            "copy path '{missing}' does not exist under {}",
            seed_from.display()
        );
    }

    clear_previous_attempt(repo, path, branch).await?;
    git::add_worktree(repo, path, branch, base_sha).await?;

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

    Ok(NodeWorkspace {
        path: path.to_path_buf(),
        branch: branch.to_string(),
        seeded: copy_paths.to_vec(),
    })
}

/// Remove what a previous attempt at this node left behind: the checkout a
/// failure deliberately kept, and the branch under it.
///
/// A re-run supersedes the earlier attempt, and git will reuse neither name
/// while they exist — without this, resuming a run that failed inside an agent
/// node would fail again on the worktree instead of on the work.
async fn clear_previous_attempt(repo: &Path, path: &Path, branch: &str) -> anyhow::Result<()> {
    // A worktree whose directory is already gone still holds its
    // administrative entry, which is enough to make the name unusable.
    git::prune_worktrees(repo).await?;

    if path.exists() {
        git::remove_worktree(repo, path).await?;
    }
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
pub async fn commit(ws: &NodeWorkspace, message: &str) -> anyhow::Result<Option<String>> {
    git::commit_all_except(&ws.path, message, &ws.seeded).await
}

/// Remove the checkout. The branch survives, because it is the record of what
/// the node did.
///
/// # Errors
///
/// Returns an error if git cannot remove the worktree.
pub async fn discard(repo: impl AsRef<Path>, ws: &NodeWorkspace) -> anyhow::Result<()> {
    git::remove_worktree(repo, &ws.path).await
}
