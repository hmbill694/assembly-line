//! Where a job's state lives on disk.
//!
//! # Errors
//!
//! Functions here fail only for the usual filesystem reasons — a directory
//! that cannot be created or read, a file that cannot be written. Only the
//! functions whose failure means something *beyond* that document it
//! individually.

#![allow(clippy::missing_errors_doc)]

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

/// File inside a repository's worktree directory naming the repository it
/// belongs to.
const REPOSITORY_MARKER: &str = "repo";

/// FNV-1a, written out rather than taken from `DefaultHasher`, whose output is
/// explicitly unspecified across Rust releases. A worktree path that moved
/// under a toolchain upgrade would orphan every job already on disk.
fn stable_hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// A directory name identifying one repository.
///
/// Job ids restart at 1 in every repository, so a job id alone cannot key a
/// worktree directory — the first job of two different repos would claim the
/// same path. The slug leads with the repository's own directory name so the
/// tree stays browsable, and ends with a hash of its absolute path so two
/// repositories sharing a name stay apart.
#[must_use]
pub fn repo_slug(repo: &Path) -> String {
    let name: String = repo
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("repo")
        .chars()
        .map(
            |c| match c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                true => c,
                false => '-',
            },
        )
        .collect();

    format!(
        "{name}-{:016x}",
        stable_hash(repo.as_os_str().as_encoded_bytes())
    )
}

/// Environment variable that moves every worktree somewhere other than
/// `$HOME` — a faster disk, or a directory a test owns outright.
///
/// Worktrees live under `$HOME` by default, never inside the repository: the
/// target repo must stay untouched, and a worktree inside it would need a
/// `.gitignore` entry assembly-line is not entitled to add.
pub const WORKTREE_ROOT_VAR: &str = "ASSEMBLY_WORKTREE_ROOT";

/// Where worktrees go, given what the environment says. The override wins
/// outright and is used verbatim.
///
/// Split from the lookup below because reading the environment is not
/// something a test can do twice: `set_var` is process-global and unsafe under
/// edition 2024, so the rule stays testable only while it is a function of its
/// arguments.
#[must_use]
pub fn worktrees_root_given(
    override_root: Option<impl Into<PathBuf>>,
    home: Option<impl Into<PathBuf>>,
) -> Option<PathBuf> {
    match override_root {
        Some(elsewhere) => Some(elsewhere.into()),
        None => home.map(|home| home.into().join(".assembly").join("wt")),
    }
}

/// Every repository's worktrees.
///
/// `None` only when neither [`WORKTREE_ROOT_VAR`] nor `$HOME` is set, which
/// the caller should report rather than guessing a location.
#[must_use]
pub fn worktrees_root() -> Option<PathBuf> {
    worktrees_root_given(
        std::env::var_os(WORKTREE_ROOT_VAR),
        std::env::var_os("HOME"),
    )
}

/// One repository's worktrees, across all of its jobs. This is the level `gc`
/// walks, and where the repository marker lives.
#[must_use]
pub fn repo_worktrees_root(repo: &Path) -> Option<PathBuf> {
    worktrees_root().map(|root| root.join(repo_slug(repo)))
}

/// One job's worktrees.
#[must_use]
pub fn worktree_root(repo: &Path, job_id: u64) -> Option<PathBuf> {
    repo_worktrees_root(repo).map(|root| root.join(job_id.to_string()))
}

/// Record which repository a worktree directory belongs to, returning that
/// directory.
///
/// The slug carries a hash, so it cannot be read backwards. Without this
/// marker `gc` could only collect leftovers for the repository it happens to
/// be run from; with it, every repository's are reachable from one place.
///
/// # Errors
///
/// Beyond the usual, an error when `$HOME` is unset.
pub fn record_repository_for_worktrees(repo: &Path) -> io::Result<PathBuf> {
    let dir = repo_worktrees_root(repo)
        .ok_or_else(|| io::Error::other("HOME is unset, so worktrees have nowhere to live"))?;
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join(REPOSITORY_MARKER),
        repo.as_os_str().as_encoded_bytes(),
    )?;
    Ok(dir)
}

/// The repository a worktree directory belongs to, or `None` when it carries
/// no marker — an empty or hand-made directory, which `gc` leaves alone.
#[must_use]
pub fn repository_owning_worktrees(repo_worktrees_root: &Path) -> Option<PathBuf> {
    std::fs::read_to_string(repo_worktrees_root.join(REPOSITORY_MARKER))
        .ok()
        .map(|path| PathBuf::from(path.trim_end_matches('\n')))
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

/// The id a new job should claim. A missing jobs directory is not an error —
/// it means this is the first job.
pub fn next_job_id(jobs_root: &Path) -> io::Result<u64> {
    existing_job_ids(jobs_root).map(|ids| ids.into_iter().max().unwrap_or(0) + 1)
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

    /// Everything the agent printed, across every round — one job, one log.
    #[must_use]
    pub fn log(&self) -> PathBuf {
        self.dir.join("job.log")
    }

    /// Where the job's scratch checkout lives, one level below
    /// [`worktree_root`] rather than being it — so removing the checkout
    /// leaves the job's own directory for `gc` to find and report.
    #[must_use]
    pub fn worktree(&self, repo: &Path) -> Option<PathBuf> {
        worktree_root(repo, self.id).map(|root| root.join("checkout"))
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
