//! What a daemon starting on a root owes the jobs it finds there — decided
//! from each job's log alone.

use crate::job::JobId;
use crate::paths::{self, JobPlace, RepoKey};
use crate::report::JobReport;
use crate::runner::RoundHandle;
use crate::state::JobState;
use anyhow::Context;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resumption {
    /// Asked for and never started: back in the queue.
    Requeue,
    /// Launched and never judged: collect it from where the log left off.
    Reattach { round: u32, handle: RoundHandle },
    /// Started but never recorded as launched: the daemon was killed
    /// mid-launch, and there is no handle to find the round by.
    LostWhileLaunching { round: u32 },
}

impl Resumption {
    #[must_use]
    pub fn for_report(report: &JobReport) -> Option<Resumption> {
        match (report.state, &report.launched) {
            (JobState::Queued, _) => Some(Resumption::Requeue),
            (JobState::Running, Some(handle)) => Some(Resumption::Reattach {
                round: report.rounds,
                handle: handle.clone(),
            }),
            (JobState::Running, None) => Some(Resumption::LostWhileLaunching {
                round: report.rounds,
            }),
            (JobState::Pending | JobState::Passed | JobState::Failed, _) => None,
        }
    }
}

/// Why a round a daemon started, and never recorded as launched, has failed.
/// A daemon stopping gives its launches up and says so; one that finds this
/// was killed mid-launch.
pub(super) fn lost_while_launching(job: JobId) -> String {
    format!(
        "the daemon was killed while this round was launching, so it cannot be found again — \
         check the runner for a stray round, then `submit --job {job}` to try again"
    )
}

/// Every job under `root` that has a log, with its report. Jobs are found
/// where [`JobPlace::create`] puts them, `<root>/jobs/<repository>/<id>`,
/// and the key comes from the repository directory's name. A name that
/// [`RepoKey::from_dir_name`] or a job id does not make is passed over.
///
/// # Errors
///
/// When a jobs directory cannot be listed or a log cannot be read.
pub fn jobs_under(root: &Path) -> anyhow::Result<Vec<(JobPlace, JobReport)>> {
    let jobs = root.join("jobs");
    repositories_under(&jobs)
        .with_context(|| format!("reading {}", jobs.display()))?
        .into_iter()
        .map(|key| {
            let jobs_dir = key.jobs_dir(root);
            paths::existing_job_ids(&jobs_dir)
                .with_context(|| format!("reading {}", jobs_dir.display()))
                .map(|ids| {
                    ids.into_iter()
                        .map(|id| JobPlace {
                            key: key.clone(),
                            id,
                        })
                        .collect::<Vec<_>>()
                })
        })
        .collect::<anyhow::Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .filter(|place| place.paths(root).events().is_file())
        .map(|place| place.paths(root).report().map(|report| (place, report)))
        .collect()
}

fn repositories_under(jobs: &Path) -> std::io::Result<Vec<RepoKey>> {
    match std::fs::read_dir(jobs) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
        Ok(entries) => entries
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()
            .map(|dirs| {
                dirs.iter()
                    .filter(|dir| dir.is_dir())
                    .filter_map(|dir| RepoKey::from_dir_name(dir.file_name()?.to_str()?))
                    .collect()
            }),
    }
}
