//! The host's side of a job's stream: frames in, `events.jsonl` and the log
//! out. Runner-agnostic — it sees lines and a termination, nothing else.

use crate::event::{Event, EventKind, EventLog};
use crate::frame::{Routed, StreamPosition, verdict_missing_from};
use crate::job::JobOutcome;
use crate::runner::RunningJob;
use std::io::Write;
use std::path::Path;
use tokio_util::sync::CancellationToken;

/// Collect one round's stream until it ends, then settle its outcome.
///
/// # Errors
///
/// Returns an error only if the log or the event log cannot be written. A
/// job that fails, or dies without saying how, is a [`JobOutcome::Failed`].
/// The job is cancelled, and its end waited for, before the error is
/// returned: dropping it would kill only the local end — a `docker` client,
/// or `job-exec` before it has stopped its agent — and leave the work
/// running with nobody collecting it.
pub async fn collect<J: RunningJob>(
    mut job: J,
    log: &mut EventLog,
    output_log: &Path,
    round: u32,
    cancel: CancellationToken,
) -> anyhow::Result<JobOutcome> {
    let collected = match stream_into_logs(&mut job, log, output_log, cancel).await {
        Ok(collected) => collected,
        Err(e) => {
            cancel_and_wait_out(job).await;
            return Err(e);
        }
    };

    let termination = job.termination().await;
    let settled = start_missing_from(&collected, round)
        .into_iter()
        .chain(verdict_missing_from(&collected, &termination.to_string()))
        .map(|kind| log.append(kind))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(outcome_of(&[collected, settled].concat()))
}

/// The start of a round whose stream never announced one — a job that died
/// before its first frame, or never ran. Without it the round's failure would
/// read as the previous round's, and the next revise would reuse its number.
fn start_missing_from(round_events: &[Event], round: u32) -> Option<EventKind> {
    (!round_events
        .iter()
        .any(|e| matches!(e.kind, EventKind::JobStarted { .. })))
    .then_some(EventKind::JobStarted { round })
}

/// Route every line of the job's stream to the event log or the output log
/// until the stream ends, returning the events collected.
///
/// A loop, not a fold: each line is I/O that must land before the next is
/// read, and cancellation arrives from outside mid-stream.
async fn stream_into_logs<J: RunningJob>(
    job: &mut J,
    log: &mut EventLog,
    output_log: &Path,
    cancel: CancellationToken,
) -> anyhow::Result<Vec<Event>> {
    let mut output = open_for_append(output_log)?;
    let mut position = StreamPosition::default();
    let mut collected: Vec<Event> = Vec::new();
    let mut cancelling = false;

    loop {
        let line = tokio::select! {
            line = job.next_line() => line,
            () = cancel.cancelled(), if !cancelling => {
                job.cancel().await;
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
    Ok(collected)
}

/// Cancel `job` and wait for it to end, discarding the rest of its stream so
/// it never blocks writing to a pipe nobody reads.
async fn cancel_and_wait_out<J: RunningJob>(mut job: J) {
    job.cancel().await;
    // A loop: each line is awaited in turn until the stream ends.
    while job.next_line().await.is_some() {}
    let _ = job.termination().await;
}

/// A runner that could not start the job at all still leaves a record: the
/// round began, it failed, and this is why.
///
/// # Errors
///
/// Returns an error if the event log cannot be written.
pub fn record_launch_failure(
    log: &mut EventLog,
    round: u32,
    error: &anyhow::Error,
) -> anyhow::Result<JobOutcome> {
    log.append(EventKind::JobStarted { round })?;
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
