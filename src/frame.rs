//! The wire format between a round and whoever collects it.
//!
//! A round runs somewhere its collector cannot see — a child process, a
//! container, a pod on another machine — and the one thing all of those
//! share is stdout. Every line a round prints there is a [`Frame`]: one of
//! its [`Event`]s, or one line of what its commands printed.
//!
//! The round wraps its commands' output itself, so nothing an agent *prints*
//! can arrive as an `event` frame: an agent echoing `{"t":"round_passed"}`
//! lands in the log as text, not in the event stream as a verdict. That is
//! the whole guarantee — an agent running as the round's own user can still
//! write to the round's stdout directly, which the spec lists as an accepted
//! risk.

use crate::event::{Event, EventKind};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frame {
    /// Position in the round's stream, from 1. A collector that has to
    /// reconnect replays from an earlier point, and `seq` is what lets it
    /// drop what it already has.
    pub seq: u64,
    #[serde(flatten)]
    pub body: FrameBody,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameBody {
    Event(Event),
    /// One line a command printed, on stdout or stderr.
    Output(String),
}

#[derive(Debug)]
struct Numbered<W> {
    sink: W,
    last_seq: u64,
    events: Vec<Event>,
}

/// The round's side of the stream. Cloning shares it: the round appends events
/// while the readers forwarding its commands' stdout and stderr append
/// output, and all of them draw from one sequence.
#[derive(Debug)]
pub struct FrameWriter<W: Write> {
    shared: Arc<Mutex<Numbered<W>>>,
}

impl<W: Write> Clone for FrameWriter<W> {
    fn clone(&self) -> Self {
        FrameWriter {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<W: Write> FrameWriter<W> {
    pub fn new(sink: W) -> Self {
        FrameWriter {
            shared: Arc::new(Mutex::new(Numbered {
                sink,
                last_seq: 0,
                events: Vec::new(),
            })),
        }
    }

    /// Append one event, stamped now, and flush it.
    ///
    /// # Errors
    ///
    /// Returns an error if the frame cannot be written or flushed. The caller
    /// should treat this as fatal: an event that never left the round is an
    /// event the collector can never record.
    pub fn append_event(&self, kind: EventKind) -> io::Result<Event> {
        let event = Event {
            at: Utc::now(),
            kind,
        };
        self.append(FrameBody::Event(event.clone()))?;
        Ok(event)
    }

    /// Append one line a command printed.
    ///
    /// # Errors
    ///
    /// Returns an error if the frame cannot be written or flushed.
    pub fn append_output(&self, line: &str) -> io::Result<()> {
        self.append(FrameBody::Output(line.to_string()))
    }

    fn append(&self, body: FrameBody) -> io::Result<()> {
        let mut numbered = self
            .shared
            .lock()
            .map_err(|_| io::Error::other("a frame writer panicked mid-write"))?;
        let frame = Frame {
            seq: numbered.last_seq + 1,
            body,
        };
        let line = serde_json::to_string(&frame).map_err(io::Error::other)?;
        numbered.sink.write_all(line.as_bytes())?;
        numbered.sink.write_all(b"\n")?;
        numbered.sink.flush()?;
        numbered.last_seq = frame.seq;
        if let FrameBody::Event(event) = frame.body {
            numbered.events.push(event);
        }
        Ok(())
    }

    /// Every event this writer has sent, in order — what the round reported,
    /// for a conclusion drawn in the same process.
    ///
    /// # Panics
    ///
    /// Panics if a writer panicked while holding the lock.
    #[must_use]
    pub fn events_so_far(&self) -> Vec<Event> {
        self.shared
            .lock()
            .expect("frame writer lock")
            .events
            .clone()
    }

    /// What has been written so far — for tests, which write into a `Vec`.
    ///
    /// # Panics
    ///
    /// Panics if a writer panicked while holding the lock.
    #[must_use]
    pub fn copy_of_sink(&self) -> W
    where
        W: Clone,
    {
        self.shared.lock().expect("frame writer lock").sink.clone()
    }

    /// The sink, once this is the last clone of the writer.
    #[must_use]
    pub fn into_sink(self) -> Option<W> {
        Arc::try_unwrap(self.shared)
            .ok()
            .and_then(|numbered| numbered.into_inner().ok())
            .map(|numbered| numbered.sink)
    }
}

/// A frame sink for a person at a terminal: the text of every output frame,
/// one per line, and nothing of the events — the conclusion reports those.
#[derive(Debug)]
pub struct ReadableFrames<W: Write> {
    text: W,
    /// A line split across writes, waiting for the rest of it.
    partial: Vec<u8>,
}

impl<W: Write> ReadableFrames<W> {
    pub fn new(text: W) -> Self {
        ReadableFrames {
            text,
            partial: Vec::new(),
        }
    }

    pub fn into_text(self) -> W {
        self.text
    }

    fn write_readable(&mut self, line: &[u8]) -> io::Result<()> {
        match serde_json::from_slice::<Frame>(line) {
            Ok(Frame {
                body: FrameBody::Output(text),
                ..
            }) => writeln!(self.text, "{text}"),
            Ok(Frame {
                body: FrameBody::Event(_),
                ..
            }) => Ok(()),
            Err(_) => {
                self.text.write_all(line)?;
                self.text.write_all(b"\n")
            }
        }
    }
}

impl<W: Write> Write for ReadableFrames<W> {
    /// A loop: each complete line is written before the next is looked for.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.partial.extend_from_slice(buf);
        while let Some(end) = self.partial.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=end).collect();
            self.write_readable(&line[..end])?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.text.flush()
    }
}

/// What a collector does with one line of a round's stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Routed {
    /// Append to `events.jsonl`, as the round recorded it.
    Event { seq: u64, event: Event },
    /// Append to the job's log.
    Output(String),
    /// A frame at or before one already routed — a resumed stream replaying.
    AlreadyCollected,
}

/// How far into a round's stream a collector has got.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StreamPosition {
    last_seq: u64,
}

impl StreamPosition {
    /// Where `line` goes, and where the stream stands after it.
    ///
    /// A line that is not a frame is output: `job-exec`'s own stderr, or a
    /// crash backtrace, merged into the stream by a runner that cannot keep
    /// the two apart. It does not move the position.
    #[must_use]
    pub fn route(self, line: &str) -> (StreamPosition, Routed) {
        match serde_json::from_str::<Frame>(line) {
            Err(_) => (self, Routed::Output(line.to_string())),
            Ok(frame) if frame.seq <= self.last_seq => (self, Routed::AlreadyCollected),
            Ok(Frame { seq, body }) => (
                StreamPosition { last_seq: seq },
                match body {
                    FrameBody::Event(event) => Routed::Event { seq, event },
                    FrameBody::Output(text) => Routed::Output(text),
                },
            ),
        }
    }
}

/// The failure a collector records itself when a round's stream ended
/// without saying how it went — a pod OOM-killed, a runner that never
/// started it, a `job-exec` that panicked. `None` when the round reported its
/// own verdict.
///
/// `collected` is this round's events only: an earlier round's verdict says
/// nothing about this one.
#[must_use]
pub fn verdict_missing_from(collected: &[Event], ended_because: &str) -> Option<EventKind> {
    let reported = collected.iter().any(|e| {
        matches!(
            e.kind,
            EventKind::RoundPassed | EventKind::RoundFailed { .. }
        )
    });

    (!reported).then(|| EventKind::RoundFailed {
        reason: format!("the round ended without reporting a verdict: {ended_because}"),
    })
}
