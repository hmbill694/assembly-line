//! From what a person typed — a `--repo`, a job id, or nothing — to where
//! that job's state lives under the root.

use crate::job::JobId;
use crate::paths::{self, JobPaths, RepoKey};
use crate::payload::remote_to_clone;
use crate::workspace::DEFAULT_REMOTE;
use anyhow::anyhow;
use std::path::{Path, PathBuf};

/// Which checkout a command acts on: whatever `--repo` named, else the one
/// the user is standing in.
///
/// Every command resolves it the same way, so a job started with `--repo` is
/// findable by `status`, `logs` and `revise` with the same `--repo`.
///
/// # Errors
///
/// When nothing is named and the working directory is in no git repository.
pub fn checkout_named_or_enclosing(repo: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    match repo {
        Some(named) => Ok(named),
        None => enclosing_checkout(),
    }
}

fn enclosing_checkout() -> anyhow::Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    paths::git_root(&cwd)
        .ok_or_else(|| anyhow!("not inside a git repository — name one with --repo"))
}

/// The jobs directory for `repo` — or for the repository the user is
/// standing in — under `root`, and the remote URL it is keyed by.
///
/// # Errors
///
/// When there is no repository here, it has no remote, or the remote names
/// no directory a key could be made of.
pub async fn jobs_dir_of(root: &Path, repo: Option<PathBuf>) -> anyhow::Result<(PathBuf, String)> {
    let checkout = checkout_named_or_enclosing(repo)?;
    let remote_url = remote_to_clone(&checkout, DEFAULT_REMOTE).await?;
    let key = RepoKey::from_remote_url(&remote_url)?;
    Ok((key.jobs_dir(root), remote_url))
}

/// Job `job_id` of `repo` under `root`, or its latest when `job_id` is
/// `None`.
///
/// # Errors
///
/// As [`jobs_dir_of`]; and when there are no jobs yet, or no such job.
pub async fn job_at(
    root: &Path,
    repo: Option<PathBuf>,
    job_id: Option<u64>,
) -> anyhow::Result<JobPaths> {
    let (jobs_dir, _) = jobs_dir_of(root, repo).await?;
    let id = match job_id {
        Some(id) => JobId::from(id),
        None => paths::latest_job_id(&jobs_dir)?.ok_or_else(|| anyhow!("no jobs yet"))?,
    };
    Ok(paths::open_job(&jobs_dir, id)?)
}
