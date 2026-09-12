use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;

/// Everything that happens to one job.
///
/// A job's identity — its repository, ref, prompt and provider — lives in
/// `meta.json`, so no event repeats it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum EventKind {
    /// A round began. Round 1 is the first attempt; higher rounds are revises.
    JobStarted {
        round: u32,
    },
    /// The checkout had changes, now recorded on the job's branch.
    JobCommitted {
        sha: String,
        files: usize,
        insertions: usize,
        deletions: usize,
    },
    /// The branch was made durable. `pushed_to` names the remote it reached,
    /// or is `None` when the branch stayed a local ref — because the
    /// repository has no such remote, or because the remote refused the push.
    /// Both are complete outcomes, not degraded ones: the branch exists and
    /// holds the work.
    ///
    /// Emitted for failed jobs too, and whatever became of the push: a job
    /// leaves nothing but its branch, so this is what makes the work
    /// findable at all.
    JobBranchPublished {
        branch: String,
        pushed_to: Option<String>,
    },
    /// `verify` ran to completion and rejected the work. Recorded before
    /// [`EventKind::JobFailed`], so a reader can tell a rejected job from one
    /// whose agent crashed.
    ///
    /// `reason` is how `verify` failed — `exit 1` — not what it printed; the
    /// command's output is in the job's log. Written only for a verdict
    /// `verify` actually reached: a run that was cancelled or cut off at
    /// `max_duration` judged nothing, and this event would claim otherwise in
    /// a log that can never be corrected.
    JobVerifyFailed {
        reason: String,
    },
    JobFinished {
        exit_code: i32,
    },
    JobFailed {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub at: DateTime<Utc>,
    #[serde(flatten)]
    pub kind: EventKind,
}

/// Parse a newline-delimited event stream from any reader.
///
/// A line that does not parse is skipped, not fatal: the common cause is a
/// torn final line from a crash mid-write, and the events before it are still
/// the truth about what happened.
///
/// # Errors
///
/// Returns an error only if the underlying reader fails. Unparseable lines
/// are logged and skipped rather than failing the read.
pub fn read_events(src: impl BufRead) -> io::Result<Vec<Event>> {
    src.lines()
        .collect::<io::Result<Vec<String>>>()
        .map(|lines| {
            lines
                .iter()
                .filter(|line| !line.trim().is_empty())
                .filter_map(|line| match serde_json::from_str::<Event>(line) {
                    Ok(event) => Some(event),
                    Err(e) => {
                        tracing::warn!("skipping unreadable event line: {e}");
                        None
                    }
                })
                .collect()
        })
}

/// Append-only event sink. The log is the source of truth for a job; it is
/// never rewritten or truncated.
///
/// Generic over its sink so tests can write into a buffer, defaulting to the
/// on-disk file that real runs use.
#[derive(Debug)]
pub struct EventLog<W: Write = File> {
    sink: W,
}

impl<W: Write> EventLog<W> {
    pub fn new(sink: W) -> Self {
        EventLog { sink }
    }

    /// Append one event and flush, so a crash cannot lose it.
    ///
    /// # Errors
    ///
    /// Returns an error if the event cannot be serialised, or if the write or
    /// flush fails. The caller should treat this as fatal: state that is not
    /// in the log cannot be replayed.
    pub fn append(&mut self, kind: EventKind) -> io::Result<Event> {
        let event = Event {
            at: Utc::now(),
            kind,
        };
        let line = serde_json::to_string(&event).map_err(io::Error::other)?;
        self.sink.write_all(line.as_bytes())?;
        self.sink.write_all(b"\n")?;
        self.sink.flush()?;
        Ok(event)
    }

    pub fn sink(&self) -> &W {
        &self.sink
    }
}

impl EventLog<File> {
    /// Open a log for appending, creating it and its parent if needed.
    ///
    /// # Errors
    ///
    /// Returns an error if the parent directory cannot be created or the file
    /// cannot be opened for appending. The file is never truncated.
    pub fn open_append(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        path.parent()
            .map(std::fs::create_dir_all)
            .transpose()
            .and_then(|_| OpenOptions::new().create(true).append(true).open(path))
            .map(EventLog::new)
    }

    /// Read a log from disk. A missing file is an empty log, not an error —
    /// a job that has not written its first event yet is a legitimate state.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be read. Individual
    /// unparseable lines are skipped rather than failing the whole read.
    pub fn read(path: impl AsRef<Path>) -> io::Result<Vec<Event>> {
        match File::open(path.as_ref()) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e),
            Ok(file) => read_events(BufReader::new(file)),
        }
    }
}
