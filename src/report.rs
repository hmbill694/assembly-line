use crate::event::{Event, EventKind};
use crate::state::JobState;
use chrono::{DateTime, Utc};
use std::time::Duration;

/// How much a job changed, as recorded when its work was committed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffSummary {
    pub files: usize,
    pub insertions: usize,
    pub deletions: usize,
}

impl std::fmt::Display for DiffSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} file{} +{}/-{}",
            self.files,
            match self.files == 1 {
                true => "",
                false => "s",
            },
            self.insertions,
            self.deletions
        )
    }
}

/// Everything a job's event log says about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobReport {
    pub id: u64,
    pub state: JobState,
    /// How many rounds this job has had. 1 unless it has been revised.
    pub rounds: u32,
    /// Wall time of the most recent round.
    pub duration: Option<Duration>,
    /// What the job committed, or `None` if the agent changed nothing.
    pub diff: Option<DiffSummary>,
    /// Failure reason, when there is one.
    pub detail: Option<String>,
    /// The branch the job's work is on, once it has been published. This is
    /// the job's whole durable output, so a report without it is a job that
    /// has not produced anything yet.
    pub branch: Option<String>,
}

/// A job's facts, accumulated as its event stream is folded.
#[derive(Debug, Clone, Default)]
struct JobProgress {
    state: JobState,
    rounds: u32,
    attempt_started_at: Option<DateTime<Utc>>,
    last_attempt_duration: Option<Duration>,
    committed_diff: Option<DiffSummary>,
    detail: Option<String>,
    branch: Option<String>,
}

impl JobProgress {
    /// Fold one event in. Written to be passed directly to `Iterator::fold`.
    fn after_event(self, event: &Event) -> Self {
        match &event.kind {
            // A new round restarts the clock and clears the previous round's
            // reason and diff, so a revised job reports its final round.
            EventKind::JobStarted { round } => JobProgress {
                state: JobState::Running,
                rounds: (*round).max(self.rounds + 1),
                attempt_started_at: Some(event.at),
                committed_diff: None,
                detail: None,
                ..self
            },
            EventKind::JobCommitted {
                files,
                insertions,
                deletions,
                ..
            } => JobProgress {
                committed_diff: Some(DiffSummary {
                    files: *files,
                    insertions: *insertions,
                    deletions: *deletions,
                }),
                ..self
            },
            // Publishing moves a ref, not the job's own progress — but it is
            // where the branch's name enters the record.
            EventKind::JobBranchPublished { branch, .. } => JobProgress {
                branch: Some(branch.clone()),
                ..self
            },
            // A progress marker, like `JobCommitted` — the transition to
            // `Failed` comes from the `JobFailed` that always follows it.
            // Setting `detail` here only matters if that invariant is ever
            // broken; when it holds, `JobFailed`'s reason overwrites it.
            EventKind::JobVerifyFailed { reason } => JobProgress {
                detail: Some(format!("verify rejected the work: {reason}")),
                ..self
            },
            EventKind::JobFinished { .. } => JobProgress {
                state: JobState::Succeeded,
                last_attempt_duration: self.time_spent_until(event.at),
                ..self
            },
            EventKind::JobFailed { reason, .. } => JobProgress {
                state: JobState::Failed,
                last_attempt_duration: self.time_spent_until(event.at),
                detail: Some(reason.clone()),
                ..self
            },
        }
    }

    fn time_spent_until(&self, end: DateTime<Utc>) -> Option<Duration> {
        self.attempt_started_at
            .and_then(|start| (end - start).to_std().ok())
    }
}

impl JobReport {
    /// Fold a job's event stream into the account `status` prints.
    ///
    /// A job with no events yet reports as `Pending` with one round, which is
    /// what a directory allocated but not yet run looks like.
    pub fn from_events<'a>(id: u64, events: impl IntoIterator<Item = &'a Event>) -> Self {
        let progress = events
            .into_iter()
            .fold(JobProgress::default(), JobProgress::after_event);

        JobReport {
            id,
            state: progress.state,
            rounds: progress.rounds.max(1),
            duration: progress.last_attempt_duration,
            diff: progress.committed_diff,
            detail: progress.detail,
            branch: progress.branch,
        }
    }

    /// The single line printed at the end of a job:
    /// `job 7: failed (round 2, 3 files +40/-2) — verify failed`.
    #[must_use]
    pub fn to_summary_line(&self) -> String {
        let facts = [
            Some(format!("round {}", self.rounds)),
            self.diff.map(|d| d.to_string()),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ");

        format!(
            "job {}: {} ({facts}){}",
            self.id,
            self.state.label(),
            self.detail
                .as_ref()
                .map(|why| format!(" — {why}"))
                .unwrap_or_default(),
        )
    }

    /// How long the last round took, as `status` prints it. `None` for a job
    /// whose last round has not ended.
    #[must_use]
    pub fn to_duration_line(&self) -> Option<String> {
        self.duration
            .map(|d| format!("took {:.1}s", d.as_secs_f64()))
    }
}
