//! Where a job's state lives on disk.
//!
//! # Errors
//!
//! Functions here fail only for the usual filesystem reasons — a directory
//! that cannot be created or read, a file that cannot be written. Only the
//! functions whose failure means something *beyond* that document it
//! individually.

#![allow(clippy::missing_errors_doc)]

use crate::job::JobId;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

/// `.git` is a directory in a normal repo and a file in a worktree checkout,
/// so this tests for either.
#[must_use]
pub fn git_root(from: &Path) -> Option<PathBuf> {
    from.ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
}

#[must_use]
pub fn jobs_root(git_root: &Path) -> PathBuf {
    git_root.join(".assembly").join("jobs")
}

fn existing_job_ids(jobs_root: &Path) -> io::Result<Vec<u64>> {
    match std::fs::read_dir(jobs_root) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
        Ok(entries) => entries
            .map(|e| e.map(|e| e.file_name().to_str().and_then(|s| s.parse::<u64>().ok())))
            .collect::<io::Result<Vec<_>>>()
            .map(|ids| ids.into_iter().flatten().collect()),
    }
}

/// The id a new job should claim: one past every id already taken, whether
/// by a job directory here or by a job branch on the remote.
///
/// The remote counts because job branches are shared there — a second clone,
/// a teammate, or a deleted `.assembly/jobs` would otherwise restart at 1 and
/// push onto somebody else's branch. Branches that are not a job's are
/// ignored. `None` when a branch has already taken the last id there is.
#[must_use]
pub fn job_id_past(
    local_ids: impl IntoIterator<Item = u64>,
    remote_branches: &[String],
) -> Option<u64> {
    local_ids
        .into_iter()
        .chain(
            remote_branches
                .iter()
                .filter_map(|branch| JobId::from_branch_name(branch).map(u64::from)),
        )
        .max()
        .unwrap_or(0)
        .checked_add(1)
}

/// [`job_id_past`] the job directories under `jobs_root` and
/// `remote_branches`. A missing jobs directory is not an error — it means
/// this is the first job here.
///
/// # Errors
///
/// Also fails when a job branch has taken the last id there is.
pub fn next_job_id(jobs_root: &Path, remote_branches: &[String]) -> io::Result<u64> {
    existing_job_ids(jobs_root).and_then(|ids| {
        job_id_past(ids, remote_branches).ok_or_else(|| {
            io::Error::other(format!(
                "a job branch has taken the last job id — delete {} from the remote",
                JobId::from(u64::MAX).branch_name()
            ))
        })
    })
}

/// The most recent job, or `None` when there have been none.
pub fn latest_job_id(jobs_root: &Path) -> io::Result<Option<u64>> {
    existing_job_ids(jobs_root).map(|ids| ids.into_iter().max())
}

#[derive(Debug, Clone)]
pub struct JobPaths {
    pub id: u64,
    pub dir: PathBuf,
}

impl JobPaths {
    #[must_use]
    pub fn events(&self) -> PathBuf {
        self.dir.join("events.jsonl")
    }

    #[must_use]
    pub fn meta(&self) -> PathBuf {
        self.dir.join("meta.json")
    }

    /// Everything the job's commands printed, across every round — one job,
    /// one log.
    #[must_use]
    pub fn log(&self) -> PathBuf {
        self.dir.join("job.log")
    }
}

/// Create the directory layout for a new job.
pub fn create_job(jobs_root: &Path, id: u64) -> io::Result<JobPaths> {
    let dir = jobs_root.join(id.to_string());
    std::fs::create_dir_all(&dir)?;
    Ok(JobPaths { id, dir })
}

/// Locate an existing job. `NotFound` rather than an empty result, so
/// `revise` and `status` can report a wrong job id.
pub fn open_job(jobs_root: &Path, id: u64) -> io::Result<JobPaths> {
    let dir = jobs_root.join(id.to_string());
    match dir.is_dir() {
        true => Ok(JobPaths { id, dir }),
        false => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no such job: {id}"),
        )),
    }
}

/// Enough to reconstruct a job from its id alone — `revise` needs the prompt
/// it is revising, and the ref it was cut from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobMeta {
    pub repo: PathBuf,
    pub base_ref: String,
    pub prompt: String,
    pub provider: String,
}

pub fn write_meta(paths: &JobPaths, meta: &JobMeta) -> io::Result<()> {
    serde_json::to_string_pretty(meta)
        .map_err(io::Error::other)
        .and_then(|body| std::fs::write(paths.meta(), body))
}

/// Invalid JSON here means the job directory was written by an incompatible
/// version, or hand-edited.
pub fn read_meta(paths: &JobPaths) -> io::Result<JobMeta> {
    std::fs::read_to_string(paths.meta())
        .and_then(|body| serde_json::from_str(&body).map_err(io::Error::other))
}
