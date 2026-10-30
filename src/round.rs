//! Running one round of a job: a repository, a start commit and a prompt.
//!
//! A round is stateless. Its checkout is a scratch clone and is discarded
//! whatever happened, including on failure; the job's branch, pushed to the
//! remote, is the whole durable output — which is why work is committed and
//! pushed *before* the verdict is reached, and why a round that cannot push
//! fails.

use crate::event::EventKind;
use crate::exec::{ShellOutcome, run_command, run_shell};
use crate::frame::FrameWriter;
use crate::git;
use crate::payload::RoundPayload;
use crate::provider::CommandSpec;
use crate::workspace::{self, PROVISIONING_LIMIT, RoundWorkspace, ScratchClone};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// `mise trust` first: `mise` refuses to act on a `mise.toml` in a directory
/// it has not been told to trust, and a fresh clone is exactly that.
const PROVISIONING_STEPS: [&[&str]; 2] = [&["trust", "--all", "--yes"], &["install", "--yes"]];

/// How a round ended: whether its work was accepted. `verify` decides this
/// when the repository declares one; otherwise the agent's exit code does. A
/// round that crashed, timed out or lost its push failed too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Passed,
    Failed,
}

impl Verdict {
    #[must_use]
    pub fn passed(self) -> bool {
        matches!(self, Self::Passed)
    }
}

/// A round's verdict and the events that record it — always the last a
/// round reports.
#[derive(Debug)]
struct RoundClosing {
    verdict: Verdict,
    events: Vec<EventKind>,
}

impl RoundClosing {
    fn passed() -> Self {
        Self {
            verdict: Verdict::Passed,
            events: vec![EventKind::RoundPassed],
        }
    }

    fn failed_because(reason: String) -> Self {
        Self {
            verdict: Verdict::Failed,
            events: vec![EventKind::RoundFailed { reason }],
        }
    }
}

/// A round whose work is recorded but whose verdict is not, holding its
/// checkout until [`FinishedRound::close`].
#[derive(Debug)]
#[must_use = "a round reports its verdict and discards its checkout only when closed"]
pub struct FinishedRound {
    closing: RoundClosing,
    branch_tip: String,
    workspace: Option<RoundWorkspace>,
}

impl FinishedRound {
    #[must_use]
    pub fn verdict(&self) -> Verdict {
        self.closing.verdict
    }

    /// The commit the round left the job's branch at on the remote: its own,
    /// or the one it started from when it pushed nothing.
    #[must_use]
    pub fn branch_tip(&self) -> &str {
        &self.branch_tip
    }

    /// The round's checkout, which is where `gh` runs. `None` for a round
    /// that failed before it had one.
    #[must_use]
    pub fn workspace_path(&self) -> Option<&Path> {
        self.workspace.as_ref().map(RoundWorkspace::path)
    }

    /// The same round, failed for `reason` whatever it ended as.
    pub fn failed_because(self, reason: String) -> Self {
        Self {
            closing: RoundClosing::failed_because(reason),
            ..self
        }
    }

    /// Report the verdict, then discard the checkout.
    ///
    /// # Errors
    ///
    /// When the frames cannot be written.
    pub fn close<W: Write>(self, frames: &FrameWriter<W>) -> std::io::Result<Verdict> {
        let FinishedRound {
            closing, workspace, ..
        } = self;
        append_all(frames, closing.events)?;
        workspace.map_or(Ok(()), |ws| discard_reporting_failure(ws, frames))?;
        Ok(closing.verdict)
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

/// How a round ended, and the work it left. Every variant that can carry work
/// does carry it: a round is judged after its work is preserved, never
/// instead of.
#[derive(Debug)]
enum RoundResult {
    /// An agent that correctly decided nothing needed doing.
    PassedUnchanged,
    PassedWithWork {
        work: AgentWork,
    },
    /// `verify` rejected a round the agent finished cleanly.
    Rejected {
        reason: String,
        work: Option<AgentWork>,
    },
    Failed {
        reason: String,
        work: Option<AgentWork>,
    },
}

impl RoundResult {
    fn work(&self) -> Option<&AgentWork> {
        match self {
            Self::PassedUnchanged => None,
            Self::PassedWithWork { work } => Some(work),
            Self::Rejected { work, .. } | Self::Failed { work, .. } => work.as_ref(),
        }
    }
}

/// Remove the round's checkout. That is housekeeping: failing at it neither
/// undoes a pushed branch nor says anything about the work, so it is
/// reported as output rather than allowed to replace what the round did.
fn discard_reporting_failure<W: Write>(
    ws: RoundWorkspace,
    frames: &FrameWriter<W>,
) -> std::io::Result<()> {
    match workspace::discard(ws) {
        Ok(()) => Ok(()),
        Err(e) => frames.append_output(&format!("could not remove the scratch checkout: {e}")),
    }
}

/// One round in `clone`: check out the job's branch, provision, agent,
/// commit, push, `verify`, and the events that record its work. No
/// `RoundStarted`: whoever collects the round numbers it. The verdict waits
/// in the [`FinishedRound`] until it is closed.
///
/// `provisioning_deadline` is what is left of the budget the clone and
/// provisioning share, fixed before the clone began.
///
/// # Errors
///
/// Only when the round's frames cannot be written. A checkout that cannot be
/// started, an agent that fails and a push that fails all finish the round
/// as [`Verdict::Failed`].
pub async fn run_round<W: Write + Send + 'static>(
    clone: ScratchClone,
    payload: &RoundPayload,
    frames: &FrameWriter<W>,
    provisioning_deadline: Instant,
    cancel: CancellationToken,
) -> anyhow::Result<FinishedRound> {
    match workspace::start_round(clone, &payload.start, &payload.branch).await {
        Ok(ws) => {
            let (branch_tip, closing) =
                run_round_in(payload, &ws, frames, provisioning_deadline, cancel).await?;
            Ok(FinishedRound {
                closing,
                branch_tip,
                workspace: Some(ws),
            })
        }
        Err(e) => Ok(FinishedRound {
            closing: RoundClosing::failed_because(e.to_string()),
            branch_tip: payload.start.sha.clone(),
            workspace: None,
        }),
    }
}

/// The round in `ws`, its work events written, and the commit it left the
/// branch at with its closing events, unwritten.
async fn run_round_in<W: Write + Send + 'static>(
    payload: &RoundPayload,
    ws: &RoundWorkspace,
    frames: &FrameWriter<W>,
    provisioning_deadline: Instant,
    cancel: CancellationToken,
) -> anyhow::Result<(String, RoundClosing)> {
    let timeout = payload.command_limit_secs.map(Duration::from_secs);
    let result = result_in_workspace(payload, ws, frames, provisioning_deadline, timeout, cancel)
        .await
        // The round could not be administered at all — provisioning failed,
        // a git step before the push could not run, or the push failed and
        // took the work with it. Either way there is no branch to name.
        .unwrap_or_else(|e| RoundResult::Failed {
            reason: e.to_string(),
            work: None,
        });

    let branch_tip = result
        .work()
        .map_or_else(|| payload.start.sha.clone(), |work| work.sha.clone());
    let (work, closing) = events_for_completion(result);
    append_all(frames, work)?;
    Ok((branch_tip, closing))
}

/// One round in a checkout it is handed.
///
/// The agent's work is committed and published *before* success is decided:
/// an unrecorded change would be a lost change, and a failure that produced a
/// diff is exactly the case where the diff is worth reading.
async fn result_in_workspace<W: Write + Send + 'static>(
    payload: &RoundPayload,
    ws: &RoundWorkspace,
    frames: &FrameWriter<W>,
    provisioning_deadline: Instant,
    timeout: Option<Duration>,
    cancel: CancellationToken,
) -> anyhow::Result<RoundResult> {
    provision_toolchain(
        payload,
        ws.path(),
        frames,
        provisioning_deadline,
        cancel.clone(),
    )
    .await?;

    // Taken once, up front: it decides both whether `verify` is worth running
    // and how the round ends, and an agent failure wins over either answer.
    let agent_failure = agent_failure_reason(
        run_command(&payload.command, ws.path(), frames, timeout, cancel.clone()).await,
    );

    let preserved = agent_work_on_branch(payload, ws).await;
    let ruling = match agent_failure {
        Some(_) => VerifyRuling::NoObjection,
        None => {
            verify_ruling(
                payload.verify.as_deref(),
                ws.path(),
                frames,
                timeout,
                cancel,
            )
            .await
        }
    };
    let work = preserved?;

    Ok(match (agent_failure, ruling) {
        // Neither of these is a judgement about the work, so neither becomes
        // a `VerifyRejected`.
        (Some(reason), _) | (None, VerifyRuling::Interrupted { reason }) => {
            RoundResult::Failed { reason, work }
        }
        (None, VerifyRuling::Rejected { reason }) => RoundResult::Rejected { reason, work },
        (None, VerifyRuling::NoObjection) => match work {
            None => RoundResult::PassedUnchanged,
            Some(work) => RoundResult::PassedWithWork { work },
        },
    })
}

/// Install the repository's toolchain in `cwd`, when the payload asks.
///
/// Each step's `run_command` timeout is what remains of `deadline` — the
/// clone and provisioning's shared budget, fixed once before the clone — so
/// a step that runs out of time expires through `exec::supervise`'s own
/// `TimedOut` path, which kills its whole process group. Not an outer
/// `tokio::time::timeout`: that would drop `run_command` from outside, and
/// `kill_on_drop` signals `mise`'s own pid, orphaning anything it spawned. A
/// step started with no budget left fails immediately as timed out without
/// being run at all.
async fn provision_toolchain<W: Write + Send + 'static>(
    payload: &RoundPayload,
    cwd: &Path,
    frames: &FrameWriter<W>,
    deadline: Instant,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    if !payload.provision_toolchain {
        return Ok(());
    }
    // A loop: each step is sequential I/O, and a failure ends it.
    for args in PROVISIONING_STEPS {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let spec = CommandSpec {
            program: "mise".into(),
            args: args.iter().map(|a| (*a).to_string()).collect(),
        };
        let outcome = if remaining.is_zero() {
            ShellOutcome::TimedOut
        } else {
            run_command(&spec, cwd, frames, Some(remaining), cancel.clone()).await?
        };
        match (outcome, outcome.failure_reason()) {
            (_, None) => {}
            (ShellOutcome::TimedOut, Some(_)) => anyhow::bail!(
                "provisioning timed out after {}",
                humantime::format_duration(PROVISIONING_LIMIT)
            ),
            (_, Some(reason)) => anyhow::bail!(
                "provisioning the toolchain failed: `mise {}` {reason}",
                args.join(" ")
            ),
        }
    }
    Ok(())
}

/// What `verify` had to say about the tree the branch carries.
#[derive(Debug)]
enum VerifyRuling {
    /// Nothing `verify` holds against the round: it exited 0, the repository
    /// configures no `verify`, or the agent had already failed and there was
    /// nothing left worth judging.
    NoObjection,
    /// `verify` ran to completion and exited non-zero — a judgement about the
    /// work, and the only thing that gates delivery.
    Rejected { reason: String },
    /// `verify` never got to judge anything: it could not be started, was
    /// cancelled, or was cut off at `max_duration`. Ctrl-C during a long test
    /// suite is ordinary, and recording it as a rejection would put a claim
    /// about the work into an append-only log that can never be corrected.
    Interrupted { reason: String },
}

/// What `verify` made of what the agent left, run in the round's own checkout
/// after the commit, so it judges exactly the tree the branch carries.
///
/// The outcome is matched here rather than flattened through
/// `ShellOutcome::failure_reason`, which cannot tell `exit 1` from a kill.
async fn verify_ruling<W: Write + Send + 'static>(
    verify: Option<&str>,
    workspace: &Path,
    frames: &FrameWriter<W>,
    timeout: Option<Duration>,
    cancel: CancellationToken,
) -> VerifyRuling {
    let Some(command) = verify else {
        return VerifyRuling::NoObjection;
    };

    match run_shell(command, workspace, frames, timeout, cancel).await {
        Ok(ShellOutcome::Exited(0)) => VerifyRuling::NoObjection,
        Ok(ShellOutcome::Exited(code)) => VerifyRuling::Rejected {
            reason: format!("exit {code}"),
        },
        Ok(ShellOutcome::TimedOut) => VerifyRuling::Interrupted {
            reason: "verify timed out".to_string(),
        },
        Ok(ShellOutcome::Cancelled) => VerifyRuling::Interrupted {
            reason: "verify cancelled".to_string(),
        },
        // A ruling rather than an error: the branch is already on the
        // remote, and an error would take the round's record of it down too.
        Err(e) => VerifyRuling::Interrupted {
            reason: format!("verify could not run: {e}"),
        },
    }
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
    payload: &RoundPayload,
    ws: &RoundWorkspace,
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

fn append_all<W: Write>(frames: &FrameWriter<W>, events: Vec<EventKind>) -> std::io::Result<()> {
    events
        .into_iter()
        .try_for_each(|kind| frames.append_event(kind).map(|_| ()))
}

/// Empty for an agent that correctly changed nothing.
fn work_recorded(work: Option<&AgentWork>) -> Vec<EventKind> {
    work.map(|w| {
        vec![
            EventKind::RoundCommitted {
                sha: w.sha.clone(),
                files: w.stat.files,
                insertions: w.stat.insertions,
                deletions: w.stat.deletions,
            },
            EventKind::BranchPushed {
                branch: w.branch.clone(),
                pushed_to: w.pushed_to.clone(),
            },
        ]
    })
    .unwrap_or_default()
}

/// The events a completion implies, in the order things settled: those
/// recording the work, then those closing the round with the verdict they
/// add up to.
///
/// A verdict is the last event a round reports. A restarted daemon replays
/// each job's log and owes nothing to a round with a verdict, so it would
/// never reattach to collect an event the round reported after one.
fn events_for_completion(result: RoundResult) -> (Vec<EventKind>, RoundClosing) {
    match result {
        RoundResult::PassedUnchanged => (Vec::new(), RoundClosing::passed()),
        RoundResult::PassedWithWork { work } => {
            (work_recorded(Some(&work)), RoundClosing::passed())
        }
        RoundResult::Rejected { reason, work } => (
            work_recorded(work.as_ref()),
            RoundClosing {
                verdict: Verdict::Failed,
                events: vec![
                    EventKind::VerifyRejected {
                        reason: reason.clone(),
                    },
                    EventKind::RoundFailed {
                        reason: format!("verify rejected the work: {reason}"),
                    },
                ],
            },
        ),
        RoundResult::Failed { reason, work } => (
            work_recorded(work.as_ref()),
            RoundClosing::failed_because(reason),
        ),
    }
}

#[cfg(test)]
mod provisioning_tests {
    //! `provision_toolchain` is private, and the deadline it takes
    //! cannot be exercised through the public `run` surface without
    //! either waiting out the real 15-minute `PROVISIONING_LIMIT` or making
    //! it configurable — both ruled out. This calls the internal function
    //! directly instead, which needs no subprocess: a deadline that has
    //! already passed must fail the very first step without ever running
    //! `mise`, so no fake binary or `PATH` juggling is needed either.

    use super::*;
    use crate::git::PinnedRef;

    fn payload_asking_for_provisioning() -> RoundPayload {
        RoundPayload {
            job_id: 1.into(),
            remote_url: "does-not-matter".to_string(),
            remote_name: "origin".to_string(),
            start: PinnedRef {
                name: "main".to_string(),
                sha: "0".repeat(40),
            },
            branch: "al/job-1".to_string(),
            command: CommandSpec {
                program: "true".to_string(),
                args: Vec::new(),
            },
            commit_message: "job 1".to_string(),
            verify: None,
            command_limit_secs: None,
            provision_toolchain: true,
        }
    }

    #[tokio::test]
    async fn a_deadline_already_passed_fails_the_next_step_without_running_it() {
        let payload = payload_asking_for_provisioning();
        let frames = FrameWriter::new(Vec::new());
        // In the past, so `saturating_duration_since` has already floored
        // at zero by the time the first step checks it.
        let deadline = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .expect("the process has been up for over a second");

        let result = provision_toolchain(
            &payload,
            Path::new("/nonexistent-provisioning-test-cwd"),
            &frames,
            deadline,
            CancellationToken::new(),
        )
        .await;

        let error = result.expect_err(
            "a step given no remaining budget must fail as timed out, not run `mise` \
             against a `cwd` that does not even exist",
        );
        assert!(
            error.to_string().contains("provisioning timed out"),
            "{error}"
        );
    }
}
