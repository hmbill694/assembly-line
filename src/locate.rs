//! From what a person typed — a `--repo`, a job id, or nothing — to where
//! that job's state lives under the root.

use crate::event::EventLog;
use crate::job::JobId;
use crate::paths::{self, JobPaths, RepoKey};
use crate::payload::remote_to_clone;
use crate::report::JobReport;
use crate::workspace::DEFAULT_REMOTE;
use anyhow::anyhow;
use std::path::{Path, PathBuf};

/// Which checkout a command acts on: whatever `--repo` named, else the one
/// the user is standing in.
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

/// The remote a command acts on: `--repo` itself when it names no checkout,
/// else the checkout's — the one named, or the one the user is standing in.
///
/// Every command resolves it the same way, so a job submitted with `--repo`
/// is findable by `status`, `logs` and `submit --job` with the same `--repo`.
///
/// # Errors
///
/// When there is no checkout here, or it has no remote.
pub async fn remote_url_named_or_enclosing(repo: Option<String>) -> anyhow::Result<String> {
    match repo {
        Some(url) if !Path::new(&url).join(".git").exists() => Ok(url),
        named => {
            let checkout = checkout_named_or_enclosing(named.map(PathBuf::from))?;
            remote_to_clone(&checkout, DEFAULT_REMOTE).await
        }
    }
}

/// The jobs directory for `repo` — or for the repository the user is
/// standing in — under `root`.
///
/// # Errors
///
/// As [`remote_url_named_or_enclosing`]; and when the remote names no
/// directory a key could be made of.
pub async fn jobs_dir_of(root: &Path, repo: Option<String>) -> anyhow::Result<PathBuf> {
    let remote_url = remote_url_named_or_enclosing(repo).await?;
    Ok(RepoKey::from_remote_url(&remote_url)?.jobs_dir(root))
}

/// Job `job_id` of `repo` under `root`, or its latest when `job_id` is
/// `None`.
///
/// # Errors
///
/// As [`jobs_dir_of`]; and when there are no jobs yet, or no such job.
pub async fn job_at(
    root: &Path,
    repo: Option<String>,
    job_id: Option<u64>,
) -> anyhow::Result<JobPaths> {
    let jobs_dir = jobs_dir_of(root, repo).await?;
    let id = match job_id {
        Some(id) => JobId::from(id),
        None => paths::latest_job_id(&jobs_dir)?.ok_or_else(|| anyhow!("no jobs yet"))?,
    };
    Ok(paths::open_job(&jobs_dir, id)?)
}

/// The report on the job `job_id` names in `repo`, or on the latest one there
/// when it names none.
///
/// # Errors
///
/// When the job cannot be found, or its event log cannot be read.
pub async fn report_for_job(
    root: &Path,
    job_id: Option<u64>,
    repo: Option<String>,
) -> anyhow::Result<JobReport> {
    let paths = job_at(root, repo, job_id).await?;
    let events =
        EventLog::read(paths.events()).map_err(|e| anyhow!("reading the event log: {e}"))?;
    Ok(JobReport::from_events(paths.id.into(), &events))
}

/// Where job `job_id` in `repo` captured its output.
///
/// # Errors
///
/// When the job cannot be found, or has captured nothing yet.
pub async fn output_log_of(
    root: &Path,
    job_id: u64,
    repo: Option<String>,
) -> anyhow::Result<PathBuf> {
    let paths = job_at(root, repo, Some(job_id)).await?;
    let log = paths.log();
    match log.exists() {
        true => Ok(log),
        false => Err(anyhow!("job {job_id} has captured no output yet")),
    }
}
