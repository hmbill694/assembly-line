use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Ok,
    Partial,
    Aborted,
}

impl RunStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Partial => "partial",
            Self::Aborted => "aborted",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum EventKind {
    RunStarted { run_id: u64, jobs: usize },
    NodeStarted { node: String, round: u32 },
    NodeFinished { node: String, exit_code: i32 },
    NodeFailed { node: String, reason: String },
    NodeSkipped { node: String, because: String },
    RunFinished { status: RunStatus },
}

impl EventKind {
    /// The node this event concerns, if any.
    pub fn node(&self) -> Option<&str> {
        match self {
            Self::NodeStarted { node, .. }
            | Self::NodeFinished { node, .. }
            | Self::NodeFailed { node, .. }
            | Self::NodeSkipped { node, .. } => Some(node),
            Self::RunStarted { .. } | Self::RunFinished { .. } => None,
        }
    }
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

/// Append-only event sink. The log is the source of truth for a run; it is
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
    pub fn open_append(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        path.parent()
            .map(std::fs::create_dir_all)
            .transpose()
            .and_then(|_| OpenOptions::new().create(true).append(true).open(path))
            .map(EventLog::new)
    }

    /// Read a log from disk. A missing file is an empty log, not an error —
    /// a run that has not written its first event yet is a legitimate state.
    pub fn read(path: impl AsRef<Path>) -> io::Result<Vec<Event>> {
        match File::open(path.as_ref()) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e),
            Ok(file) => read_events(BufReader::new(file)),
        }
    }
}
