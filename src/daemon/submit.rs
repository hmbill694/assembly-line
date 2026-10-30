//! A submit, as the daemon takes it: preflight in its own bare cache, then
//! claim and record. Nothing is claimed for a job that would be refused.

use super::Serving;
use super::api::{Queued, Refused, Submission};
use crate::admission::{Admitted, admit};
use crate::claim::claim_job;
use crate::event::EventKind;
use crate::git;
use crate::job::JobId;
use crate::paths::{JobPaths, JobPlace, RepoKey};
use crate::report::JobRequests;
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
    let cache = key.repo_cache(root);
    let _repo = serving.repos.hold(&key).await;
    git::init_bare_if_absent(&cache)
        .await
        .map_err(|e| unpreparable(format!("preparing {}: {e}", cache.display())))?;

    let continued = match submission.job {
        Some(id) => {
            let place = JobPlace {
                key: key.clone(),
                id: JobId::from(id),
            };
            Some(continued_job(&place, root, &cache, &url).await?)
        }
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
    let admitted = admit(&cache, base, provider_named.as_deref())
        .await
        .map_err(|e| refused(e.to_string(), e.itemized_reasons()))?;

    let job = match continued {
        Some(job) => job.paths,
        None => new_job(&key, root, &cache, &url, &admitted).await?,
    };
    let id = job.id;
    let warnings = admitted
        .settings()
        .settings_worth_flagging()
        .iter()
        .map(ToString::to_string)
        .collect();
    let (base, settings) = admitted.into_base_and_settings();
    job.append(EventKind::RoundRequested {
        remote_url: url,
        base,
        prompt: submission.prompt,
        provider: settings.provider_name,
    })
    .map_err(|e| unpreparable(format!("recording job {id}'s request: {e}")))?;
    serving.queue.enqueue(JobPlace { key, id });

    Ok(Queued {
        job: id.into(),
        branch: id.branch_name(),
        warnings,
    })
}

/// A new job's directory, under the id it claimed on `url` at its admitted
/// base.
async fn new_job(
    key: &RepoKey,
    root: &Path,
    cache: &Path,
    url: &str,
    admitted: &Admitted,
) -> Result<JobPaths, Refused> {
    let id = claim_job(cache, url, admitted)
        .await
        .map_err(unpreparable)?;
    let place = JobPlace {
        key: key.clone(),
        id,
    };
    place.create(root).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => unpreparable(format!(
            "job {id}'s branch was gone from {url}, so its id was claimed again, but {} still \
             holds that job — submit again, and the next id will be claimed",
            place.paths(root).dir.display()
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
    place: &JobPlace,
    root: &Path,
    cache: &Path,
    url: &str,
) -> Result<Continued, Refused> {
    let id = place.id;
    let paths = place.open(root).map_err(unpreparable)?;
    let report = paths.report().map_err(|e| unpreparable(format!("{e:#}")))?;
    if matches!(report.state, JobState::Queued | JobState::Running) {
        return Err(unpreparable(format!(
            "job {id} is already queued or running — wait for its verdict before revising it"
        )));
    }
    let Some(JobRequests { latest, .. }) = report.requests else {
        return Err(unpreparable(format!(
            "job {id} has no recorded request in its event log, so there is nothing to revise; \
             submit without --job to start a new job"
        )));
    };
    match git::remote_lacks_ref(cache, url, &id.branch_name()).await {
        Ok(true) => Err(unpreparable(format!(
            "job {id} has no branch on {url} — it was deleted, or the job never pushed one, so \
             there is nothing to revise; submit without --job to start a new job"
        ))),
        Ok(false) => Ok(Continued {
            paths,
            base_ref: latest.base.name,
            provider: latest.provider,
        }),
        Err(e) => Err(unpreparable(e)),
    }
}
