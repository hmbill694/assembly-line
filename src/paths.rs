//! Where a job's state lives on disk: under a state root, in a directory
//! per repository and a directory per job.
//!
//! # Errors
//!
//! Functions here fail only for the usual filesystem reasons — a directory
//! that cannot be created or read, a file that cannot be written. Only the
//! functions whose failure means something *beyond* that document it
//! individually.

#![allow(clippy::missing_errors_doc)]

use crate::job::JobId;
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

/// Where job state lives: `named` when there is one, else the XDG state
/// directory, else its conventional place under `$HOME`.
///
/// # Errors
///
/// When nothing is named and there is no `$HOME` to default under.
pub fn state_root(
    named: Option<PathBuf>,
    env: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<PathBuf> {
    match (named, env("XDG_STATE_HOME"), env("HOME")) {
        (Some(named), _, _) => Ok(named),
        // The XDG spec has a relative value ignored: it would put state under
        // whatever directory the command ran in — the target checkout.
        (None, Some(xdg), _) if Path::new(&xdg).is_absolute() => {
            Ok(PathBuf::from(xdg).join("assembly-line"))
        }
        (None, _, Some(home)) if !home.is_empty() => {
            Ok(PathBuf::from(home).join(".local/state/assembly-line"))
        }
        (None, _, _) => Err(anyhow::anyhow!(
            "no $HOME to keep job state under — name a directory with --root or $ASSEMBLY_ROOT"
        )),
    }
}

/// A repository's place under the state root: its host, then the path
/// segments of its remote URL, from the URL's HTTPS form — so the SSH and
/// HTTPS spellings of one repository are one key. A repository on this
/// machine has the host `local`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RepoKey {
    segments: Vec<String>,
}

/// A remote URL that names no directory a key could be made of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnkeyableRemote {
    pub url: String,
}

impl std::fmt::Display for UnkeyableRemote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the remote '{}' does not name a repository assembly-line can keep state for — \
             use an absolute path, or a URL whose segments are letters, digits, '.', '_' and '-'",
            self.url
        )
    }
}

impl std::error::Error for UnkeyableRemote {}

impl RepoKey {
    /// # Errors
    ///
    /// When a segment is empty, `.` or `..`, or holds anything but ASCII
    /// letters, digits, `.`, `_` and `-` — validated, never sanitized.
    pub fn from_remote_url(url: &str) -> Result<RepoKey, UnkeyableRemote> {
        let unkeyable = || UnkeyableRemote {
            url: url.to_string(),
        };
        let https = crate::payload::https_equivalent(url);
        let (host, path) = match https.split_once("://") {
            Some(("file", path)) => ("local".to_string(), path.to_string()),
            Some((_, rest)) => {
                let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
                let host = authority.rsplit('@').next().unwrap_or(authority);
                let host = crate::git::split_at_host_colon(host).map_or(host, |(host, _)| host);
                (host.to_ascii_lowercase(), path.to_string())
            }
            None if https.starts_with('/') => ("local".to_string(), https.clone()),
            None => return Err(unkeyable()),
        };
        let segments: Vec<String> = std::iter::once(host)
            .chain(
                path.trim_end_matches('/')
                    .trim_end_matches(".git")
                    .split('/')
                    .filter(|segment| !segment.is_empty())
                    .map(str::to_string),
            )
            .collect();

        match segments.len() > 1 && segments.iter().all(|s| is_keyable_segment(s)) {
            true => Ok(RepoKey { segments }),
            false => Err(unkeyable()),
        }
    }

    /// Where this repository's jobs live under `root`.
    #[must_use]
    pub fn jobs_dir(&self, root: &Path) -> PathBuf {
        self.segments
            .iter()
            .fold(root.join("jobs"), |dir, segment| dir.join(segment))
    }

    /// The daemon's bare repository for this remote, under `root`.
    ///
    /// # Panics
    ///
    /// Never: a key is only made with a host and at least one path segment.
    #[must_use]
    pub fn repo_cache(&self, root: &Path) -> PathBuf {
        let (name, parents) = self
            .segments
            .split_last()
            .expect("a key has a host and at least one path segment");
        parents
            .iter()
            .fold(root.join("repos"), |dir, segment| dir.join(segment))
            .join(format!("{name}.git"))
    }
}

impl std::fmt::Display for RepoKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.segments.join("/"))
    }
}

fn is_keyable_segment(segment: &str) -> bool {
    !matches!(segment, "" | "." | "..")
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// The ids of the jobs `jobs_dir` holds. Entries that are not a job's are
/// ignored.
pub fn existing_job_ids(jobs_dir: &Path) -> io::Result<Vec<JobId>> {
    match std::fs::read_dir(jobs_dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
        Ok(entries) => entries
            .map(|e| {
                e.map(|e| {
                    e.file_name()
                        .to_str()
                        .and_then(|s| s.parse::<u64>().ok())
                        .map(JobId::from)
                })
            })
            .collect::<io::Result<Vec<_>>>()
            .map(|ids| ids.into_iter().flatten().collect()),
    }
}

/// One past the highest job branch `remote_branches` carries. Branches that
/// are not a job's are ignored. `None` when a branch has already taken the
/// last id there is.
#[must_use]
pub fn job_id_past(remote_branches: &[String]) -> Option<JobId> {
    remote_branches
        .iter()
        .filter_map(|branch| JobId::from_branch_name(branch))
        .max()
        .map_or(0, u64::from)
        .checked_add(1)
        .map(JobId::from)
}

/// The most recent job, or `None` when there have been none.
pub fn latest_job_id(jobs_dir: &Path) -> io::Result<Option<JobId>> {
    existing_job_ids(jobs_dir).map(|ids| ids.into_iter().max())
}

#[derive(Debug, Clone)]
pub struct JobPaths {
    pub id: JobId,
    pub dir: PathBuf,
}

impl JobPaths {
    #[must_use]
    pub fn events(&self) -> PathBuf {
        self.dir.join("events.jsonl")
    }

    /// Everything the job's commands printed, across every round — one job,
    /// one log.
    #[must_use]
    pub fn log(&self) -> PathBuf {
        self.dir.join("job.log")
    }
}

/// Create the directory layout for a new job. `AlreadyExists` when the job
/// has one: its id came from the remote, and a directory left by a job whose
/// branch has since gone must not be taken over.
pub fn create_job(jobs_dir: &Path, id: JobId) -> io::Result<JobPaths> {
    let dir = jobs_dir.join(id.to_string());
    std::fs::create_dir_all(jobs_dir).and_then(|()| std::fs::create_dir(&dir))?;
    Ok(JobPaths { id, dir })
}

/// Locate an existing job. `NotFound` rather than an empty result, so
/// `submit --job` and `status` can report a wrong job id.
pub fn open_job(jobs_dir: &Path, id: JobId) -> io::Result<JobPaths> {
    let dir = jobs_dir.join(id.to_string());
    match dir.is_dir() {
        true => Ok(JobPaths { id, dir }),
        false => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no such job: {id}"),
        )),
    }
}
