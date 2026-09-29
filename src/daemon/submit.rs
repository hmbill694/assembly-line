//! A submit, as the daemon takes it: preflight in its own bare cache, then
//! claim and record. Nothing is claimed for a job that would be refused.

use super::Serving;
use super::api::{Queued, Refused, Submission};
use super::dispatch::JobAddress;
use crate::claim::claim_job;
use crate::config::{REPO_CONFIG_PATH, RepoConfig};
use crate::event::{EventKind, EventLog};
use crate::git;
use crate::job::JobId;
use crate::paths::{self, JobPaths, RepoKey};
use crate::report::JobReport;
use crate::runner::{Runner, reasons_a_container_cannot_run};
use crate::state::JobState;
use std::path::Path;

fn refused(summary: impl Into<String>, reasons: Vec<String>) -> Refused {
    Refused {
        summary: summary.into(),
        reasons,
    }
}

fn unpreparable(e: impl std::fmt::Display) -> Refused {
    refused(e.to_string(), Vec::new())
}

/// A job's own base and provider, which a revise starts from.
struct Continued {
    paths: JobPaths,
    base_ref: String,
    provider: String,
}

/// Preflight `submission`, claim its job if it is new, record the request
/// and queue it.
///
/// Holds the repository's lock throughout, so two submits to one
/// repository claim one after the other rather than race for one id.
///
/// # Errors
///
/// A [`Refused`] naming every reason the job cannot run, before anything is
/// claimed or recorded.
pub async fn accept<R: Runner>(
    serving: &Serving<R>,
    submission: Submission,
) -> Result<Queued, Refused> {
    let url = submission.remote_url;
    let key = RepoKey::from_remote_url(&url).map_err(unpreparable)?;
    let container_problems: Vec<String> = match R::RUNS_IN_A_CONTAINER {
        true => reasons_a_container_cannot_run(&url)
            .iter()
            .map(ToString::to_string)
            .collect(),
        false => Vec::new(),
    };
    if !container_problems.is_empty() {
        return Err(refused(
            "the job cannot run on this daemon's runner",
            container_problems,
        ));
    }
    let root = &serving.daemon.root;
    let jobs_dir = key.jobs_dir(root);
    let cache = key.repo_cache(root);
    let _repo = serving.repos.hold(&key).await;
    git::init_bare_if_absent(&cache)
        .await
        .map_err(|e| unpreparable(format!("preparing {}: {e}", cache.display())))?;

    let continued = match submission.job {
        Some(id) => Some(continued_job(&jobs_dir, JobId::from(id), &cache, &url).await?),
        None => None,
    };
    let (base_ref, provider_named) = match (&continued, submission.base_ref) {
        (Some(job), _) => (job.base_ref.clone(), Some(job.provider.clone())),
        (None, Some(base_ref)) => (base_ref, submission.provider),
        (None, None) => return Err(unpreparable("a new job needs a ref to start from")),
    };
    let base = git::pinned(&cache, &url, &base_ref)
        .await
        .map_err(unpreparable)?;
    // The base, never the job's branch: a round is not allowed to have
    // changed the settings that govern the next one.
    let config = RepoConfig::from_ref(&cache, &base.sha)
        .await
        .map_err(unpreparable)?;
    let provider = provider_named
        .or_else(|| config.provider.clone())
        .unwrap_or_default();
    let problems = config.reasons_it_cannot_run(&provider);
    if !problems.is_empty() {
        return Err(refused(
            format!("{REPO_CONFIG_PATH} is not runnable"),
            problems.iter().map(ToString::to_string).collect(),
        ));
    }

    let job = match continued {
        Some(job) => job.paths,
        None => new_job(&jobs_dir, &cache, &url, &base.sha).await?,
    };
    let id = job.id;
    EventLog::open_append(job.events())
        .and_then(|mut log| {
            log.append(EventKind::RoundRequested {
                remote_url: url,
                base,
                prompt: submission.prompt,
                provider,
            })
        })
        .map_err(|e| unpreparable(format!("recording job {id}'s request: {e}")))?;
    serving.queue.enqueue(JobAddress { key, jobs_dir, id });

    Ok(Queued {
        job: id.into(),
        branch: id.branch_name(),
        warnings: config
            .settings_worth_flagging()
            .iter()
            .map(ToString::to_string)
            .collect(),
    })
}

/// A new job's directory, under the id it claimed on `url` at `base_sha`.
async fn new_job(
    jobs_dir: &Path,
    cache: &Path,
    url: &str,
    base_sha: &str,
) -> Result<JobPaths, Refused> {
    let id = claim_job(cache, url, base_sha)
        .await
        .map_err(unpreparable)?;
    paths::create_job(jobs_dir, id).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => unpreparable(format!(
            "job {id}'s branch was gone from {url}, so its id was claimed again, but {} still \
             holds that job — submit again, and the next id will be claimed",
            jobs_dir.join(id.to_string()).display()
        )),
        _ => unpreparable(format!("preparing job {id}'s directory: {e}")),
    })
}

/// The job a revise continues, once it is known to exist, to be idle, and to
/// have a branch to continue.
///
/// A job's branch can be absent rather than unpushed — deleted once its pull
/// request merged — and "push it first" would be the wrong advice.
async fn continued_job(
    jobs_dir: &Path,
    id: JobId,
    cache: &Path,
    url: &str,
) -> Result<Continued, Refused> {
    let paths = paths::open_job(jobs_dir, id).map_err(unpreparable)?;
    let events = EventLog::read(paths.events()).map_err(unpreparable)?;
    let report = JobReport::from_events(id.into(), &events);
    if matches!(report.state, JobState::Queued | JobState::Running) {
        return Err(unpreparable(format!(
            "job {id} is already queued or running — wait for its verdict before revising it"
        )));
    }
    let (Some(base), Some(provider)) = (report.base, report.provider) else {
        return Err(unpreparable(format!(
            "job {id} has no recorded request — its first round never started, so there is \
             nothing to revise"
        )));
    };
    match git::remote_lacks_ref(cache, url, &id.branch_name()).await {
        Ok(true) => Err(unpreparable(format!(
            "job {id} has no branch on {url} — it was deleted, or the job never pushed one, so \
             there is nothing to revise; submit without --job to start a new job"
        ))),
        Ok(false) => Ok(Continued {
            paths,
            base_ref: base.name,
            provider,
        }),
        Err(e) => Err(unpreparable(e)),
    }
}
