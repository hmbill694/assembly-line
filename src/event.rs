use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;

/// Everything that happens to one job.
///
/// A job's identity — its repository, base, prompt and provider — is its
/// first [`EventKind::RoundRequested`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum EventKind {
    /// A round was asked for: the job's first, or a revise.
    RoundRequested {
        remote_url: String,
        /// The base as it was when the round was asked for. A revise
        /// re-pins it at the remote's tip then.
        base: crate::git::PinnedRef,
        prompt: String,
        provider: String,
    },
    /// A round began. Round 1 starts the job; each revise adds one.
    RoundStarted {
        round: u32,
    },
    /// The round is running on its runner, and this is how to find it again.
    RoundLaunched {
        handle: crate::runner::RoundHandle,
    },
    /// The round made commits — the agent's own, or assembly-line's of what
    /// it left uncommitted — and `sha` is the one the job's branch ends on.
    RoundCommitted {
        sha: String,
        files: usize,
        insertions: usize,
        deletions: usize,
    },
    /// The branch reached the remote. `pushed_to` names it.
    ///
    /// Emitted for failed rounds too. A job leaves nothing but its branch, so
    /// this is what makes the work findable at all.
    BranchPushed {
        branch: String,
        pushed_to: String,
    },
    /// The job's branch has a pull request, opened by the round that just
    /// passed or already open from an earlier one.
    PullRequestOpened {
        url: String,
    },
    /// `verify` ran to completion and rejected the work. Recorded before
    /// [`EventKind::RoundFailed`], so a reader can tell a rejected round from
    /// one whose agent crashed.
    ///
    /// `reason` is how `verify` failed — `exit 1` — not what it printed; the
    /// command's output is in the job's log. Written only for a ruling
    /// `verify` actually reached: a run that was cancelled or cut off at
    /// `max_duration` judged nothing, and this event would claim otherwise in
    /// a log that can never be corrected.
    VerifyRejected {
        reason: String,
    },
    RoundPassed,
    RoundFailed {
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
/// A line that does not parse is skipped, not fatal: a torn final line from a
/// crash mid-write must not cost us the events before it.
///
/// # Errors
///
/// Returns an error only if the underlying reader fails.
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
#[derive(Debug)]
pub struct EventLog<W: Write = File> {
    sink: W,
}

impl<W: Write> EventLog<W> {
    pub fn new(sink: W) -> Self {
        EventLog { sink }
    }

    /// Append one event and flush, so a crashing *process* cannot lose it.
    /// There is no `fsync`, so a machine that loses power still can.
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
        self.write_line(&event)?;
        Ok(event)
    }

    /// Append an event exactly as a round recorded it, keeping its own
    /// timestamp: the collector's copy of the round's event, not a new one.
    ///
    /// # Errors
    ///
    /// As [`EventLog::append`].
    pub fn append_collected(&mut self, event: &Event) -> io::Result<()> {
        self.write_line(event)
    }

    fn write_line(&mut self, event: &Event) -> io::Result<()> {
        let line = serde_json::to_string(event).map_err(io::Error::other)?;
        self.sink.write_all(line.as_bytes())?;
        self.sink.write_all(b"\n")?;
        self.sink.flush()
    }

    pub fn sink(&self) -> &W {
        &self.sink
    }
}

impl EventLog<File> {
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
