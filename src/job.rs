//! Running one job: a repository, a ref and a prompt.
//!
//! A job is stateless. Its checkout is a scratch clone and is discarded
//! whatever happened, including on failure; its branch, pushed to the
//! remote, is the whole durable output — which is why work is committed and
//! pushed *before* success is decided, and why a round that cannot push
//! fails.

use crate::event::{EventKind, EventLog};
use crate::exec::{ShellOutcome, run_command, run_shell};
use crate::git;
use crate::payload::JobPayload;
use crate::workspace::{self, JobWorkspace};
use std::path::Path;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Whether a job's work was accepted. `verify` decides this when the
/// repository declares one; otherwise the agent's exit code does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobOutcome {
    Passed,
    Failed,
}

impl JobOutcome {
    #[must_use]
    pub fn passed(self) -> bool {
        matches!(self, Self::Passed)
    }
}

/// What an agent left behind, recorded on the job's branch.
#[derive(Debug)]
struct AgentWork {
    branch: String,
    sha: String,
    stat: git::DiffStat,
    /// The remote the branch reached. A round that could not push has no
    /// `AgentWork` at all.
    pushed_to: String,
}

/// How a round ended. Every variant that can carry work does carry it: a
/// round is judged after its work is preserved, never instead of.
#[derive(Debug)]
enum RoundResult {
    /// An agent that correctly decided nothing needed doing.
    Succeeded,
    Committed {
        work: AgentWork,
    },
    /// `verify` rejected an otherwise-successful round.
    VerifyRejected {
        reason: String,
        work: Option<AgentWork>,
    },
    Failed {
        reason: String,
        work: Option<AgentWork>,
    },
}

/// Run one round of a job end to end: scratch clone, agent, commit, push,
/// `verify`, discard.
///
/// # Errors
///
/// Returns an error only if the round cannot be *administered* — the event
/// log cannot be appended to. An agent that fails, a clone that fails, and a
/// push that fails are all [`JobOutcome::Failed`], recorded in the log.
pub async fn run_round(
    payload: &JobPayload,
    log: &mut EventLog,
    log_path: &Path,
    scratch_root: &Path,
    cancel: CancellationToken,
) -> anyhow::Result<JobOutcome> {
    let timeout = payload.command_limit_secs.map(Duration::from_secs);

    log.append(EventKind::JobStarted {
        round: payload.round,
    })?;

    let result = round_result(payload, log_path, scratch_root, timeout, cancel)
        .await
        // The round could not be administered at all — the clone failed, or
        // the push did and took the work with it. Either way there is no
        // branch to name.
        .unwrap_or_else(|e| RoundResult::Failed {
            reason: e.to_string(),
            work: None,
        });

    record_completion(log, result)
}

/// What a round settled to, in the order it settled: the agent's work
/// preserved on the branch, the verdict on it, then the scratch checkout's
/// removal.
///
/// Three held `Result`s rather than three `?`s. The checkout is scratch
/// holding whatever was seeded into it, so leaving it behind because an
/// earlier git call failed is not an option — and building this as one
/// value leaves nowhere to put a `?` that would skip the discard.
#[derive(Debug)]
struct RoundSettlement {
    preserved: anyhow::Result<Option<AgentWork>>,
    verdict: anyhow::Result<VerifyVerdict>,
    discarded: anyhow::Result<()>,
}

impl RoundSettlement {
    /// What the round produced and what was made of it — answered only once
    /// the checkout is gone.
    fn work_and_verdict(self) -> anyhow::Result<(Option<AgentWork>, VerifyVerdict)> {
        let work = self.preserved?;
        let verdict = self.verdict?;
        self.discarded?;
        Ok((work, verdict))
    }
}

/// One round, from empty checkout to discarded checkout.
///
/// The agent's work is committed and published *before* success is decided:
/// an unrecorded change would be a lost change, and a failure that produced a
/// diff is exactly the case where the diff is worth reading.
async fn round_result(
    payload: &JobPayload,
    log_path: &Path,
    scratch_root: &Path,
    timeout: Option<Duration>,
    cancel: CancellationToken,
) -> anyhow::Result<RoundResult> {
    let ws = workspace::create(
        &payload.remote_url,
        &payload.start,
        &payload.branch,
        &payload.seed_from,
        &payload.copy,
        scratch_root,
    )
    .await?;

    // Taken once, up front: it decides both whether `verify` is worth running
    // and how the round ends, and an agent failure wins over either answer.
    let agent_failure = agent_failure_reason(
        run_command(
            &payload.command,
            ws.path(),
            log_path,
            timeout,
            cancel.clone(),
        )
        .await,
    );

    // Bound before the literal: both borrow `ws`, which `discard` consumes.
    let preserved = agent_work_on_branch(payload, &ws).await;
    let verdict = match agent_failure {
        Some(_) => Ok(VerifyVerdict::NoObjection),
        None => {
            verify_verdict(
                payload.verify.as_deref(),
                ws.path(),
                log_path,
                timeout,
                cancel,
            )
            .await
        }
    };
    let settled = RoundSettlement {
        preserved,
        verdict,
        discarded: workspace::discard(ws).map_err(anyhow::Error::from),
    };
    let (work, verdict) = settled.work_and_verdict()?;

    Ok(match (agent_failure, verdict) {
        // Neither of these is a judgement about the work, so neither becomes
        // a `VerifyRejected`.
        (Some(reason), _) | (None, VerifyVerdict::Interrupted { reason }) => {
            RoundResult::Failed { reason, work }
        }
        (None, VerifyVerdict::Rejected { reason }) => RoundResult::VerifyRejected { reason, work },
        (None, VerifyVerdict::NoObjection) => match work {
            None => RoundResult::Succeeded,
            Some(work) => RoundResult::Committed { work },
        },
    })
}

/// What `verify` had to say about the tree the branch carries.
#[derive(Debug)]
enum VerifyVerdict {
    /// Nothing `verify` holds against the round: it exited 0, the repository
    /// configures no `verify`, or the agent had already failed and there was
    /// nothing left worth judging.
    NoObjection,
    /// `verify` ran to completion and exited non-zero — a judgement about the
    /// work, and the only thing that gates delivery.
    Rejected { reason: String },
    /// `verify` was killed before it could judge anything: cancelled, or cut
    /// off at `max_duration`. Ctrl-C during a long test suite is ordinary,
    /// and recording it as a rejection would put a claim about the work into
    /// an append-only log that can never be corrected.
    Interrupted { reason: String },
}

/// What `verify` made of what the agent left, run in the job's own checkout
/// after the commit, so it judges exactly the tree the branch carries.
///
/// The outcome is matched here rather than flattened through
/// `ShellOutcome::failure_reason`, which cannot tell `exit 1` from a kill.
async fn verify_verdict(
    verify: Option<&str>,
    workspace: &Path,
    log_path: &Path,
    timeout: Option<Duration>,
    cancel: CancellationToken,
) -> anyhow::Result<VerifyVerdict> {
    let Some(command) = verify else {
        return Ok(VerifyVerdict::NoObjection);
    };

    Ok(
        match run_shell(command, workspace, log_path, timeout, cancel).await? {
            ShellOutcome::Exited(0) => VerifyVerdict::NoObjection,
            ShellOutcome::Exited(code) => VerifyVerdict::Rejected {
                reason: format!("exit {code}"),
            },
            ShellOutcome::TimedOut => VerifyVerdict::Interrupted {
                reason: "verify timed out".to_string(),
            },
            ShellOutcome::Cancelled => VerifyVerdict::Interrupted {
                reason: "verify cancelled".to_string(),
            },
        },
    )
}

/// Why the agent failed, or `None` when it ran to a clean exit. A program that
/// could not be spawned and one that exited non-zero both fail the round, with
/// different reasons.
fn agent_failure_reason(outcome: anyhow::Result<ShellOutcome>) -> Option<String> {
    match outcome {
        Err(unspawnable) => Some(unspawnable.to_string()),
        Ok(ran) => ran.failure_reason(),
    }
}

async fn agent_work_on_branch(
    payload: &JobPayload,
    ws: &JobWorkspace,
) -> anyhow::Result<Option<AgentWork>> {
    let Some(sha) = workspace::commit(ws, &payload.commit_message).await? else {
        return Ok(None);
    };
    let stat = git::diff_stat_against(ws.path(), &payload.start.sha).await?;
    workspace::publish(ws).await?;

    Ok(Some(AgentWork {
        branch: ws.branch.clone(),
        sha,
        stat,
        pushed_to: payload.remote_name.clone(),
    }))
}

fn record_completion(log: &mut EventLog, result: RoundResult) -> anyhow::Result<JobOutcome> {
    let (events, outcome) = events_for_completion(result);

    events
        .into_iter()
        .try_for_each(|kind| log.append(kind).map(|_| ()))?;

    Ok(outcome)
}

/// Empty for an agent that correctly changed nothing.
fn work_recorded(work: Option<&AgentWork>) -> Vec<EventKind> {
    work.map(|w| {
        vec![
            EventKind::JobCommitted {
                sha: w.sha.clone(),
                files: w.stat.files,
                insertions: w.stat.insertions,
                deletions: w.stat.deletions,
            },
            EventKind::JobBranchPublished {
                branch: w.branch.clone(),
                pushed_to: Some(w.pushed_to.clone()),
            },
        ]
    })
    .unwrap_or_default()
}

/// The events a completion implies, in the order things settled, paired with
/// the outcome they add up to.
///
/// Pure, so the ordering that makes replay correct can be asserted without
/// running anything. Work is recorded before the verdict that judges it.
fn events_for_completion(result: RoundResult) -> (Vec<EventKind>, JobOutcome) {
    let finished = EventKind::JobFinished { exit_code: 0 };

    match result {
        RoundResult::Succeeded => (vec![finished], JobOutcome::Passed),
        RoundResult::Failed { reason, work } => (
            work_recorded(work.as_ref())
                .into_iter()
                .chain([EventKind::JobFailed { reason }])
                .collect(),
            JobOutcome::Failed,
        ),
        RoundResult::VerifyRejected { reason, work } => (
            work_recorded(work.as_ref())
                .into_iter()
                .chain([
                    EventKind::JobVerifyFailed {
                        reason: reason.clone(),
                    },
                    EventKind::JobFailed {
                        reason: format!("verify rejected the work: {reason}"),
                    },
                ])
                .collect(),
            JobOutcome::Failed,
        ),
        RoundResult::Committed { work } => (
            work_recorded(Some(&work))
                .into_iter()
                .chain([finished])
                .collect(),
            JobOutcome::Passed,
        ),
    }
}
