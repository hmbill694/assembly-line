use crate::event::{Event, EventKind};
use crate::git::PinnedRef;
use crate::job::JobId;
use crate::runner::RoundHandle;
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

/// One `RoundRequested`, and when it was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoundRequest {
    pub remote_url: String,
    pub base: PinnedRef,
    pub prompt: String,
    pub provider: String,
    pub requested_at: DateTime<Utc>,
}

/// What a job was asked for: its first request, whose remote is where the
/// job clones from and pushes to, and its latest, which its next round runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRequests {
    pub first: RoundRequest,
    pub latest: RoundRequest,
}

/// Everything a job's event log says about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobReport {
    pub id: JobId,
    pub state: JobState,
    /// How many rounds this job has had, which is also its latest round's
    /// number: each `RoundRequested` takes the next one, whether or not that
    /// round ever starts. A `RoundStarted` past it raises it, so that a
    /// request lost to a torn log line cannot hand its number out again.
    pub rounds: u32,
    /// Whether any round has started. Until one has, nothing can have been
    /// captured for `assembly logs` to show.
    pub any_round_started: bool,
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
    /// `None` only for a log that holds no request: empty, or torn. A
    /// revise starts from the job's branch, not from the latest base.
    pub requests: Option<JobRequests>,
    /// The URL of the branch's pull request, once one is open.
    pub pull_request: Option<String>,
    /// How to find the latest round on its runner, once it has launched.
    pub launched: Option<RoundHandle>,
}

/// A job's facts, accumulated as its event stream is folded.
#[derive(Debug, Clone, Default)]
struct JobProgress {
    state: JobState,
    rounds: u32,
    any_round_started: bool,
    round_started_at: Option<DateTime<Utc>>,
    last_round_duration: Option<Duration>,
    committed_diff: Option<DiffSummary>,
    detail: Option<String>,
    branch: Option<String>,
    requests: Option<JobRequests>,
    pull_request: Option<String>,
    launched: Option<RoundHandle>,
}

impl JobProgress {
    fn after_event(self, event: &Event) -> Self {
        match &event.kind {
            EventKind::RoundRequested {
                remote_url,
                base,
                prompt,
                provider,
            } => {
                let latest = RoundRequest {
                    remote_url: remote_url.clone(),
                    base: base.clone(),
                    prompt: prompt.clone(),
                    provider: provider.clone(),
                    requested_at: event.at,
                };
                JobProgress {
                    // What waits is the round just asked for, so the previous
                    // round's reason, diff and timing no longer describe it.
                    state: JobState::Queued,
                    rounds: self.rounds + 1,
                    round_started_at: None,
                    last_round_duration: None,
                    committed_diff: None,
                    detail: None,
                    launched: None,
                    requests: Some(match self.requests {
                        Some(JobRequests { first, .. }) => JobRequests { first, latest },
                        None => JobRequests {
                            first: latest.clone(),
                            latest,
                        },
                    }),
                    ..self
                }
            }
            // A new round restarts the clock and clears the previous round's
            // reason and diff, so a revised job reports its final round.
            EventKind::RoundStarted { round } => JobProgress {
                state: JobState::Running,
                rounds: (*round).max(self.rounds),
                any_round_started: true,
                round_started_at: Some(event.at),
                committed_diff: None,
                detail: None,
                launched: None,
                ..self
            },
            EventKind::RoundLaunched { handle } => JobProgress {
                launched: Some(handle.clone()),
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
    pub fn from_events<'a>(id: JobId, events: impl IntoIterator<Item = &'a Event>) -> Self {
        let progress = events
            .into_iter()
            .fold(JobProgress::default(), JobProgress::after_event);

        JobReport {
            id,
            state: progress.state,
            rounds: progress.rounds,
            any_round_started: progress.any_round_started,
            duration: progress.last_round_duration,
            diff: progress.committed_diff,
            detail: progress.detail,
            branch: progress.branch,
            requests: progress.requests,
            pull_request: progress.pull_request,
            launched: progress.launched,
        }
    }

    /// The job's line in `status`:
    /// `job 7: failed (round 2, 3 files +40/-2) — verify rejected the work: exit 1`.
    #[must_use]
    pub fn to_summary_line(&self) -> String {
        format!(
            "job {}: {}{}",
            self.id,
            self.state.label(),
            self.to_summary_after_label()
        )
    }

    /// What a summary line says after its state label:
    /// ` (round 2, 3 files +40/-2) — verify rejected the work: exit 1`, or
    /// nothing. A report that saw no round requested or started names no
    /// round.
    #[must_use]
    pub fn to_summary_after_label(&self) -> String {
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
        let detail = self
            .detail
            .as_ref()
            .map(|why| format!(" — {why}"))
            .unwrap_or_default();
        format!("{facts}{detail}")
    }

    /// How long the last round took, as `status` prints it. `None` for a job
    /// whose last round has not ended.
    #[must_use]
    pub fn to_duration_line(&self) -> Option<String> {
        self.duration
            .map(|d| format!("took {:.1}s", d.as_secs_f64()))
    }

    /// Everything `status` prints: the summary, how long the round took, the
    /// job's id, its branch and pull request once there are any, and the
    /// command that prints its output once a round has started. A round that
    /// fails reading its config or launching has started without capturing
    /// anything, so `logs` can still have nothing to show.
    #[must_use]
    pub fn to_status_lines(&self) -> Vec<String> {
        std::iter::once(self.to_summary_line())
            .chain(self.to_duration_line())
            .chain(std::iter::once(format!("id: {}", self.id)))
            .chain(self.work_location_lines())
            .chain(
                self.any_round_started
                    .then(|| format!("logs: assembly logs {}", self.id)),
            )
            .collect()
    }

    /// `branch: …` and `pull request: …`, once there are any.
    pub fn work_location_lines(&self) -> impl Iterator<Item = String> {
        let branch = self
            .branch
            .as_ref()
            .map(|branch| format!("branch: {branch}"));
        let pull_request = self
            .pull_request
            .as_ref()
            .map(|url| format!("pull request: {url}"));
        branch.into_iter().chain(pull_request)
    }
}
