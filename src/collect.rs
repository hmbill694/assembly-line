//! The host's side of a job's stream: frames in, `events.jsonl` and the log
//! out. Runner-agnostic — it sees lines and a termination, nothing else.

use crate::event::{Event, EventKind, EventLog};
use crate::frame::{Routed, StreamPosition, verdict_missing_from};
use crate::job::JobOutcome;
use crate::runner::local::LocalJob;
use std::io::Write;
use std::path::Path;
use tokio_util::sync::CancellationToken;

/// Collect one round's stream until it ends, then settle its outcome.
///
/// A loop, not a fold: each line is I/O that must land before the next is
/// read, and cancellation arrives from outside mid-stream.
///
/// # Errors
///
/// Returns an error only if the log or the event log cannot be written. A
/// job that fails, or dies without saying how, is a [`JobOutcome::Failed`].
pub async fn collect(
    mut job: LocalJob,
    log: &mut EventLog,
    output_log: &Path,
    cancel: CancellationToken,
) -> anyhow::Result<JobOutcome> {
    let mut output = open_for_append(output_log)?;
    let mut position = StreamPosition::default();
    let mut collected: Vec<Event> = Vec::new();
    let mut cancelling = false;

    loop {
        let line = tokio::select! {
            line = job.next_line() => line,
            () = cancel.cancelled(), if !cancelling => {
                job.cancel();
                cancelling = true;
                continue;
            }
        };
        let Some(line) = line else { break };

        let (next, routed) = position.route(&line);
        position = next;
        match routed {
            Routed::Event { event, .. } => {
                log.append_collected(&event)?;
                collected.push(event);
            }
            Routed::Output(text) => writeln!(output, "{text}")?,
            Routed::AlreadyCollected => {}
        }
    }

    let termination = job.termination().await;
    if let Some(kind) = verdict_missing_from(&collected, &termination.to_string()) {
        collected.push(log.append(kind)?);
    }
    Ok(outcome_of(&collected))
}

/// A runner that could not start the job at all still leaves a record: the
/// round failed, and this is why.
///
/// # Errors
///
/// Returns an error if the event log cannot be written.
pub fn record_launch_failure(
    log: &mut EventLog,
    error: &anyhow::Error,
) -> anyhow::Result<JobOutcome> {
    log.append(EventKind::JobFailed {
        reason: format!("the runner could not start the job: {error}"),
    })?;
    Ok(JobOutcome::Failed)
}

/// What a round's events add up to: the last verdict in them.
fn outcome_of(round: &[Event]) -> JobOutcome {
    round
        .iter()
        .rev()
        .find_map(|e| match e.kind {
            EventKind::JobFinished { .. } => Some(JobOutcome::Passed),
            EventKind::JobFailed { .. } => Some(JobOutcome::Failed),
            _ => None,
        })
        .unwrap_or(JobOutcome::Failed)
}

fn open_for_append(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
}
