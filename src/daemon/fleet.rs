//! What a daemon starting on a root owes the jobs it finds there — decided
//! from each job's log alone.

use super::dispatch::JobAddress;
use crate::event::EventLog;
use crate::job::JobId;
use crate::paths::{JobPaths, RepoKey};
use crate::report::JobReport;
use crate::runner::RoundHandle;
use crate::state::JobState;
use anyhow::Context;
use std::path::{Path, PathBuf};

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

/// Every job under `root`, with its report and the address the daemon finds
/// it by. A job directory is any directory under `<root>/jobs` holding an
/// `events.jsonl`; its key comes from the remote its log names.
///
/// # Errors
///
/// When the jobs directory cannot be walked or a log cannot be read.
pub fn jobs_under(root: &Path) -> anyhow::Result<Vec<(JobAddress, JobReport)>> {
    let jobs = root.join("jobs");
    job_dirs_below(&jobs)
        .with_context(|| format!("reading {}", jobs.display()))?
        .into_iter()
        .filter_map(|dir| address_and_report(&dir).transpose())
        .collect()
}

fn job_dirs_below(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    match std::fs::read_dir(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
        Ok(entries) => entries
            .map(|entry| {
                let path = entry?.path();
                match (path.is_dir(), path.join("events.jsonl").is_file()) {
                    (true, true) => Ok(vec![path]),
                    (true, false) => job_dirs_below(&path),
                    (false, _) => Ok(Vec::new()),
                }
            })
            .collect::<std::io::Result<Vec<_>>>()
            .map(|nested| nested.into_iter().flatten().collect()),
    }
}

/// `None` for a directory whose name is not a job id or whose log names no
/// remote — not a job this daemon can do anything for.
fn address_and_report(dir: &Path) -> anyhow::Result<Option<(JobAddress, JobReport)>> {
    let Some(id) = dir
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.parse::<u64>().ok())
    else {
        return Ok(None);
    };
    let paths = JobPaths {
        id: id.into(),
        dir: dir.to_path_buf(),
    };
    let events = EventLog::read(paths.events())
        .with_context(|| format!("reading {}", paths.events().display()))?;
    let report = JobReport::from_events(id, &events);
    let Some(key) = report
        .remote_url
        .as_deref()
        .and_then(|url| RepoKey::from_remote_url(url).ok())
    else {
        return Ok(None);
    };
    let jobs_dir = dir.parent().map(Path::to_path_buf).unwrap_or_default();
    Ok(Some((
        JobAddress {
            key,
            jobs_dir,
            id: id.into(),
        },
        report,
    )))
}
