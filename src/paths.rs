//! Where a run's state lives on disk.
//!
//! # Errors
//!
//! Functions here fail only for the usual filesystem reasons — a directory
//! that cannot be created or read, a file that cannot be written. Anything
//! beyond that is documented on the function.

use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

/// Walk up from `from` looking for a `.git` entry — a directory in a normal
/// repo, a file in a worktree checkout.
#[must_use]
pub fn git_root(from: &Path) -> Option<PathBuf> {
    from.ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
}

#[must_use]
pub fn runs_root(git_root: &Path) -> PathBuf {
    git_root.join(".assembly").join("runs")
}

/// Run directories are named by integer. Anything else in there is ignored.
fn existing_run_ids(runs_root: &Path) -> io::Result<Vec<u64>> {
    match std::fs::read_dir(runs_root) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
        Ok(entries) => entries
            .map(|e| e.map(|e| e.file_name().to_str().and_then(|s| s.parse::<u64>().ok())))
            .collect::<io::Result<Vec<_>>>()
            .map(|ids| ids.into_iter().flatten().collect()),
    }
}

/// The id a new run should claim.
///
/// # Errors
///
/// Returns an error if the runs directory exists but cannot be read. A
/// missing directory is not an error — it means this is the first run.
pub fn next_run_id(runs_root: &Path) -> io::Result<u64> {
    existing_run_ids(runs_root).map(|ids| ids.into_iter().max().unwrap_or(0) + 1)
}

/// The most recent run, or `None` when there have been none.
///
/// # Errors
///
/// Returns an error if the runs directory exists but cannot be read.
pub fn latest_run_id(runs_root: &Path) -> io::Result<Option<u64>> {
    existing_run_ids(runs_root).map(|ids| ids.into_iter().max())
}

#[derive(Debug, Clone)]
pub struct RunPaths {
    pub id: u64,
    pub dir: PathBuf,
}

impl RunPaths {
    #[must_use]
    pub fn events(&self) -> PathBuf {
        self.dir.join("events.jsonl")
    }

    #[must_use]
    pub fn meta(&self) -> PathBuf {
        self.dir.join("meta.json")
    }

    #[must_use]
    pub fn logs_dir(&self) -> PathBuf {
        self.dir.join("logs")
    }

    #[must_use]
    pub fn log(&self, node: &str) -> PathBuf {
        self.logs_dir().join(format!("{node}.log"))
    }
}

/// Create the directory layout for a new run.
///
/// # Errors
///
/// Returns an error if the run directory or its `logs/` subdirectory cannot
/// be created.
pub fn create_run(runs_root: &Path, id: u64) -> io::Result<RunPaths> {
    let dir = runs_root.join(id.to_string());
    std::fs::create_dir_all(dir.join("logs"))?;
    Ok(RunPaths { id, dir })
}

/// Locate an existing run.
///
/// # Errors
///
/// Returns `NotFound` if no directory exists for `id`, so `resume` and
/// `status` can report a wrong run id rather than an empty result.
pub fn open_run(runs_root: &Path, id: u64) -> io::Result<RunPaths> {
    let dir = runs_root.join(id.to_string());
    match dir.is_dir() {
        true => Ok(RunPaths { id, dir }),
        false => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no such run: {id}"),
        )),
    }
}

/// Enough to reconstruct a run from its id alone — `resume` and `status` need
/// the graph path, and `resume` reuses the original job cap by default.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunMeta {
    /// Path to the graph file, exactly as given on the command line.
    pub graph: PathBuf,
    pub jobs: usize,
}

/// # Errors
///
/// Returns an error if `meta.json` cannot be serialised or written.
pub fn write_meta(paths: &RunPaths, meta: &RunMeta) -> io::Result<()> {
    serde_json::to_string_pretty(meta)
        .map_err(io::Error::other)
        .and_then(|body| std::fs::write(paths.meta(), body))
}

/// # Errors
///
/// Returns an error if `meta.json` is missing or is not valid JSON — which
/// means the run directory was created by an incompatible version, or hand-
/// edited.
pub fn read_meta(paths: &RunPaths) -> io::Result<RunMeta> {
    std::fs::read_to_string(paths.meta())
        .and_then(|body| serde_json::from_str(&body).map_err(io::Error::other))
}
