use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

/// Walk up from `from` looking for a `.git` entry — a directory in a normal
/// repo, a file in a worktree checkout.
pub fn git_root(from: &Path) -> Option<PathBuf> {
    from.ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
}

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

pub fn next_run_id(runs_root: &Path) -> io::Result<u64> {
    existing_run_ids(runs_root).map(|ids| ids.into_iter().max().unwrap_or(0) + 1)
}

pub fn latest_run_id(runs_root: &Path) -> io::Result<Option<u64>> {
    existing_run_ids(runs_root).map(|ids| ids.into_iter().max())
}

#[derive(Debug, Clone)]
pub struct RunPaths {
    pub id: u64,
    pub dir: PathBuf,
}

impl RunPaths {
    pub fn events(&self) -> PathBuf {
        self.dir.join("events.jsonl")
    }
    pub fn meta(&self) -> PathBuf {
        self.dir.join("meta.json")
    }
    pub fn logs_dir(&self) -> PathBuf {
        self.dir.join("logs")
    }
    pub fn log(&self, node: &str) -> PathBuf {
        self.logs_dir().join(format!("{node}.log"))
    }
}

pub fn create_run(runs_root: &Path, id: u64) -> io::Result<RunPaths> {
    let dir = runs_root.join(id.to_string());
    std::fs::create_dir_all(dir.join("logs"))?;
    Ok(RunPaths { id, dir })
}

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

pub fn write_meta(paths: &RunPaths, meta: &RunMeta) -> io::Result<()> {
    serde_json::to_string_pretty(meta)
        .map_err(io::Error::other)
        .and_then(|body| std::fs::write(paths.meta(), body))
}

pub fn read_meta(paths: &RunPaths) -> io::Result<RunMeta> {
    std::fs::read_to_string(paths.meta())
        .and_then(|body| serde_json::from_str(&body).map_err(io::Error::other))
}
