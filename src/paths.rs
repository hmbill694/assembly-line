//! Where a job's state lives on disk: under a state root, in one directory
//! per repository, none inside another's, and a directory per job.
//!
//! # Errors
//!
//! Functions here fail only for the usual filesystem reasons — a directory
//! that cannot be created or read, a file that cannot be written. Only the
//! functions whose failure means something *beyond* that document it
//! individually.

#![allow(clippy::missing_errors_doc)]

use crate::event::{EventKind, EventLog};
use crate::job::JobId;
use crate::report::JobReport;
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

/// A repository's place under the state root, made from its remote by
/// [`crate::remote::Remote::repo_key`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RepoKey {
    segments: Vec<String>,
}

/// The host of a repository on this machine. `@` is in no keyable segment,
/// so no remote's host can be this.
pub const LOCAL_HOST: &str = "@local";

/// Joins a key's segments into one directory name. It is in no keyable
/// segment, so the join can be split again.
const DIR_NAME_SEPARATOR: char = '~';

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
    /// The key whose [`Self::dir_name`] is `name`, or `None` for a name no
    /// key makes.
    #[must_use]
    pub fn from_dir_name(name: &str) -> Option<RepoKey> {
        let mut segments = name.split(DIR_NAME_SEPARATOR);
        let host = segments.next()?;
        RepoKey::from_host_and_path(host.to_string(), segments)
    }

    /// `None` unless the host is [`LOCAL_HOST`], or a keyable segment with
    /// an optional `+` and port after it, and every path segment is keyable.
    pub(crate) fn from_host_and_path<'a>(
        host: String,
        path: impl IntoIterator<Item = &'a str>,
    ) -> Option<RepoKey> {
        let path: Vec<&str> = path.into_iter().collect();
        let keyable = (host == LOCAL_HOST || is_keyable_host(&host))
            && !path.is_empty()
            && path.iter().all(|segment| is_keyable_segment(segment));
        keyable.then(|| RepoKey {
            segments: std::iter::once(host)
                .chain(path.into_iter().map(str::to_string))
                .collect(),
        })
    }

    /// The one directory this repository's state is kept under, in
    /// `<root>/jobs` and `<root>/repos` alike.
    #[must_use]
    pub fn dir_name(&self) -> String {
        self.segments.join(&DIR_NAME_SEPARATOR.to_string())
    }

    /// Where this repository's jobs live under `root`.
    #[must_use]
    pub fn jobs_dir(&self, root: &Path) -> PathBuf {
        root.join("jobs").join(self.dir_name())
    }

    /// The daemon's bare repository for this remote, under `root`.
    #[must_use]
    pub fn repo_cache(&self, root: &Path) -> PathBuf {
        root.join("repos").join(format!("{}.git", self.dir_name()))
    }
}

impl std::fmt::Display for RepoKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.segments.join("/"))
    }
}

fn is_keyable_host(host: &str) -> bool {
    match host.split_once('+') {
        Some((name, port)) => {
            is_keyable_segment(name) && !port.is_empty() && port.chars().all(|c| c.is_ascii_digit())
        }
        None => is_keyable_segment(host),
    }
}

fn is_keyable_segment(segment: &str) -> bool {
    !matches!(segment, "" | "." | "..")
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// The ids of the jobs one repository's `jobs_dir` holds. Entries that are
/// not a job's, including names like `007` that no job is created under,
/// are ignored.
pub fn existing_job_ids(jobs_dir: &Path) -> io::Result<Vec<JobId>> {
    match std::fs::read_dir(jobs_dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
        Ok(entries) => entries
            .map(|e| {
                e.map(|e| {
                    let name = e.file_name();
                    let name = name.to_str()?;
                    let id = JobId::from(name.parse::<u64>().ok()?);
                    (id.to_string() == name).then_some(id)
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

/// Where one job's state is kept under a root. It is made of nothing but
/// the job's repository and id, so two places for one job are always equal.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JobPlace {
    pub key: RepoKey,
    pub id: JobId,
}

impl JobPlace {
    /// The job's directory under `root`, whether or not it exists.
    #[must_use]
    pub fn paths(&self, root: &Path) -> JobPaths {
        JobPaths {
            id: self.id,
            dir: self.key.jobs_dir(root).join(self.id.to_string()),
        }
    }

    /// Create the job's directory under `root`. `AlreadyExists` when the job
    /// has one: its id came from the remote, and a directory left by a job
    /// whose branch has since gone must not be taken over.
    pub fn create(&self, root: &Path) -> io::Result<JobPaths> {
        let paths = self.paths(root);
        std::fs::create_dir_all(self.key.jobs_dir(root))
            .and_then(|()| std::fs::create_dir(&paths.dir))?;
        Ok(paths)
    }

    /// The job's existing directory under `root`. `NotFound` rather than an
    /// empty result, so `submit --job` and `status` can report a wrong job
    /// id.
    pub fn open(&self, root: &Path) -> io::Result<JobPaths> {
        let paths = self.paths(root);
        match paths.dir.is_dir() {
            true => Ok(paths),
            false => Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("no such job: {}", self.id),
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct JobPaths {
    pub id: JobId,
    pub dir: PathBuf,
}

impl JobPaths {
    /// The job's log, folded. A job with no log yet reports as `Pending`.
    pub fn report(&self) -> anyhow::Result<JobReport> {
        let events = EventLog::read(self.events())
            .map_err(|e| anyhow::anyhow!("reading {}: {e}", self.events().display()))?;
        Ok(JobReport::from_events(self.id, &events))
    }

    /// Append `kind` to the job's log, creating the log if it has none.
    pub fn append(&self, kind: EventKind) -> io::Result<()> {
        EventLog::open_append(self.events())?
            .append(kind)
            .map(|_| ())
    }

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

    /// Where a local round `round` writes its frames.
    #[must_use]
    pub fn frames(&self, round: u32) -> PathBuf {
        self.dir.join(format!("round-{round}.frames"))
    }

    /// The last seq the collector routed for round `round`.
    #[must_use]
    pub fn position(&self, round: u32) -> PathBuf {
        self.dir.join(format!("round-{round}.position"))
    }
}
