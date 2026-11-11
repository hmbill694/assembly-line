//! From what a person typed — a `--repo`, a job id, or nothing — to where
//! that job's state lives under the root.

use crate::git;
use crate::job::JobId;
use crate::paths::{self, JobPaths, JobPlace, RepoKey};
use crate::remote::Remote;
use crate::report::JobReport;
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

/// What a command's `--repo` names: the remote it acts on, and the checkout
/// that remote was read from, if there is one.
#[derive(Debug, Clone)]
pub struct NamedRepo {
    pub remote: Remote,
    pub checkout: Option<PathBuf>,
}

impl NamedRepo {
    /// `--repo` itself when it names no checkout, else the checkout's
    /// remote — the checkout named, or the one the user is standing in.
    ///
    /// Every command resolves it the same way, so a job submitted with
    /// `--repo` is findable by `status`, `logs` and `submit --job` with the
    /// same `--repo`.
    ///
    /// # Errors
    ///
    /// When there is no checkout here, or it has no remote.
    pub async fn named_or_enclosing(repo: Option<String>) -> anyhow::Result<NamedRepo> {
        match repo {
            Some(url) if !Path::new(&url).join(".git").exists() => Ok(NamedRepo {
                remote: Remote::from_url(url),
                checkout: None,
            }),
            named => {
                let checkout = checkout_named_or_enclosing(named.map(PathBuf::from))?;
                Ok(NamedRepo {
                    remote: Remote::of_checkout(&checkout).await?,
                    checkout: Some(checkout),
                })
            }
        }
    }

    /// The ref a new job starts from: `named`, else the checkout's
    /// checked-out branch. A remote URL has none.
    ///
    /// # Errors
    ///
    /// When nothing is named and there is no branch to fall back on.
    pub async fn ref_to_start_from(&self, named: Option<String>) -> anyhow::Result<String> {
        match (named, &self.checkout) {
            (Some(named), _) => Ok(named),
            (None, Some(checkout)) => git::current_branch(checkout)
                .await?
                .ok_or_else(|| anyhow!("HEAD is detached — name the ref to start from with --ref")),
            (None, None) => Err(anyhow!(
                "a remote URL has no checked-out branch — name the ref to start from with --ref"
            )),
        }
    }
}

/// The URL of the remote `repo` names, as [`NamedRepo::named_or_enclosing`]
/// resolves it.
///
/// # Errors
///
/// As [`NamedRepo::named_or_enclosing`].
pub async fn remote_url_named_or_enclosing(repo: Option<String>) -> anyhow::Result<String> {
    Ok(NamedRepo::named_or_enclosing(repo)
        .await?
        .remote
        .url()
        .to_string())
}

/// The key of `repo`, or of the repository the user is standing in.
///
/// # Errors
///
/// As [`NamedRepo::named_or_enclosing`]; and when the remote names no
/// directory a key could be made of.
pub async fn repo_key_named_or_enclosing(repo: Option<String>) -> anyhow::Result<RepoKey> {
    Ok(NamedRepo::named_or_enclosing(repo)
        .await?
        .remote
        .repo_key()?)
}

/// Job `job_id` of `repo` under `root`, or its latest when `job_id` is
/// `None`.
///
/// # Errors
///
/// As [`repo_key_named_or_enclosing`]; and when there are no jobs yet, or
/// no such job.
pub async fn job_at(
    root: &Path,
    repo: Option<String>,
    job_id: Option<u64>,
) -> anyhow::Result<JobPaths> {
    let key = repo_key_named_or_enclosing(repo).await?;
    let id = match job_id {
        Some(id) => JobId::from(id),
        None => paths::latest_job_id(&key.jobs_dir(root))?.ok_or_else(|| anyhow!("no jobs yet"))?,
    };
    Ok(JobPlace { key, id }.open(root)?)
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
    job_at(root, repo, job_id).await?.report()
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
