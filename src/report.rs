use crate::event::{Event, EventKind};
use crate::git::PinnedRef;
use crate::state::JobState;
use chrono::{DateTime, Utc};
use std::time::Duration;

/// How much a round changed, as recorded when its work was committed.
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
    /// How many rounds this job has had: the highest round any
    /// `RoundStarted` records; 0 when none does.
    pub rounds: u32,
    /// Wall time of the most recent round.
    pub duration: Option<Duration>,
    /// What the latest round committed, or `None` if its agent changed
    /// nothing.
    pub diff: Option<DiffSummary>,
    /// Failure reason, when there is one.
    pub detail: Option<String>,
    /// The branch the job's work is on, once a round has pushed some. `None`
    /// is a job that has not produced anything yet, even though its claimed
    /// branch exists.
    pub branch: Option<String>,
    /// Where the job clones from and pushes to, from its first request.
    pub remote_url: Option<String>,
    /// The job's base as its latest round asked for it. A revise starts from
    /// the job's branch, not from here.
    pub base: Option<PinnedRef>,
    /// What the job was first asked to do.
    pub first_prompt: Option<String>,
    /// What the job's latest round was asked to do.
    pub latest_prompt: Option<String>,
    /// The provider its latest round was asked to use.
    pub provider: Option<String>,
    /// The URL of the branch's pull request, once one is open.
    pub pull_request: Option<String>,
}

/// A job's facts, accumulated as its event stream is folded.
#[derive(Debug, Clone, Default)]
struct JobProgress {
    state: JobState,
    rounds: u32,
    round_started_at: Option<DateTime<Utc>>,
    last_round_duration: Option<Duration>,
    committed_diff: Option<DiffSummary>,
    detail: Option<String>,
    branch: Option<String>,
    remote_url: Option<String>,
    base: Option<PinnedRef>,
    first_prompt: Option<String>,
    latest_prompt: Option<String>,
    provider: Option<String>,
    pull_request: Option<String>,
}

impl JobProgress {
    fn after_event(self, event: &Event) -> Self {
        match &event.kind {
            EventKind::RoundRequested {
                remote_url,
                base,
                prompt,
                provider,
            } => JobProgress {
                // What waits is the round just asked for, so the previous
                // round's reason, diff and timing no longer describe it.
                state: JobState::Queued,
                round_started_at: None,
                last_round_duration: None,
                committed_diff: None,
                detail: None,
                remote_url: self.remote_url.or_else(|| Some(remote_url.clone())),
                first_prompt: self.first_prompt.or_else(|| Some(prompt.clone())),
                latest_prompt: Some(prompt.clone()),
                base: Some(base.clone()),
                provider: Some(provider.clone()),
                ..self
            },
            // A new round restarts the clock and clears the previous round's
            // reason and diff, so a revised job reports its final round.
            EventKind::RoundStarted { round } => JobProgress {
                state: JobState::Running,
                rounds: (*round).max(self.rounds),
                round_started_at: Some(event.at),
                committed_diff: None,
                detail: None,
                ..self
            },
            EventKind::RoundCommitted {
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
            EventKind::BranchPushed { branch, .. } => JobProgress {
                branch: Some(branch.clone()),
                ..self
            },
            EventKind::PullRequestOpened { url } => JobProgress {
                pull_request: Some(url.clone()),
                ..self
            },
            // The `RoundFailed` that follows carries the reason, worded by
            // `round.rs`.
            EventKind::VerifyRejected { .. } => self,
            EventKind::RoundPassed => JobProgress {
                state: JobState::Passed,
                last_round_duration: self.time_spent_until(event.at),
                ..self
            },
            EventKind::RoundFailed { reason } => JobProgress {
                state: JobState::Failed,
                last_round_duration: self.time_spent_until(event.at),
                detail: Some(reason.clone()),
                ..self
            },
        }
    }

    fn time_spent_until(&self, end: DateTime<Utc>) -> Option<Duration> {
        self.round_started_at
            .and_then(|start| (end - start).to_std().ok())
    }
}

impl JobReport {
    /// Fold a job's event stream into the account `status` prints.
    ///
    /// A job with no events yet reports as `Pending` with no rounds, which is
    /// what a directory allocated but not yet run looks like.
    pub fn from_events<'a>(id: u64, events: impl IntoIterator<Item = &'a Event>) -> Self {
        let progress = events
            .into_iter()
            .fold(JobProgress::default(), JobProgress::after_event);

        JobReport {
            id,
            state: progress.state,
            rounds: progress.rounds,
            duration: progress.last_round_duration,
            diff: progress.committed_diff,
            detail: progress.detail,
            branch: progress.branch,
            remote_url: progress.remote_url,
            base: progress.base,
            first_prompt: progress.first_prompt,
            latest_prompt: progress.latest_prompt,
            provider: progress.provider,
            pull_request: progress.pull_request,
        }
    }

    /// The single line printed at the end of a round:
    /// `job 7: failed (round 2, 3 files +40/-2) — verify failed`. A report
    /// that saw no round start names no round.
    #[must_use]
    pub fn to_summary_line(&self) -> String {
        let facts: Vec<String> = [
            (self.rounds > 0).then(|| format!("round {}", self.rounds)),
            self.diff.map(|d| d.to_string()),
        ]
        .into_iter()
        .flatten()
        .collect();
        let facts = match facts.is_empty() {
            true => String::new(),
            false => format!(" ({})", facts.join(", ")),
        };

        format!(
            "job {}: {}{facts}{}",
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

    /// Everything `status` prints: the summary, how long the last round
    /// took once it has ended, and the branch and its pull request once
    /// there are any.
    #[must_use]
    pub fn to_status_lines(&self) -> Vec<String> {
        std::iter::once(self.to_summary_line())
            .chain(self.to_duration_line())
            .chain(
                self.branch
                    .as_ref()
                    .map(|branch| format!("branch: {branch}")),
            )
            .chain(
                self.pull_request
                    .as_ref()
                    .map(|url| format!("pull request: {url}")),
            )
            .collect()
    }
}
