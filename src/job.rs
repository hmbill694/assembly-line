//! Running one job: a repository, a ref and a prompt.
//!
//! A job is stateless. Its checkout is a scratch clone and is discarded
//! whatever happened, including on failure; its branch, pushed to the
//! remote, is the whole durable output — which is why work is committed and
//! pushed *before* success is decided, and why a round that cannot push
//! fails.

use crate::event::EventKind;
use crate::exec::{ShellOutcome, run_command, run_shell};
use crate::frame::FrameWriter;
use crate::git;
use crate::payload::{GIT_TOKEN_VAR, JobPayload};
use crate::provider::CommandSpec;
use crate::workspace::{self, JobWorkspace};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// How long cloning and provisioning may take together. Not `max_duration`:
/// that is the repository's statement about its own commands, and a cold
/// toolchain install should not eat the agent's budget.
const PROVISIONING_LIMIT: Duration = Duration::from_mins(15);

/// `mise trust` first: `mise` refuses to act on a `mise.toml` in a directory
/// it has not been told to trust, and a fresh clone is exactly that.
const PROVISIONING_STEPS: [&[&str]; 2] = [&["trust", "--all", "--yes"], &["install", "--yes"]];

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
/// Returns an error only if the round cannot be *administered* — its frames
/// cannot be written. An agent that fails, a clone that fails, and a push
/// that fails are all [`JobOutcome::Failed`], reported as events.
pub async fn run_round<W: Write + Send + 'static>(
    payload: &JobPayload,
    frames: &FrameWriter<W>,
    scratch_root: &Path,
    cancel: CancellationToken,
) -> anyhow::Result<JobOutcome> {
    let timeout = payload.command_limit_secs.map(Duration::from_secs);

    frames.append_event(EventKind::JobStarted {
        round: payload.round,
    })?;

    let result = round_result(payload, frames, scratch_root, timeout, cancel)
        .await
        // The round could not be administered at all — the clone or
        // provisioning failed, a git step before the push could not run, or
        // the push failed and took the work with it. Either way there is no
        // branch to name.
        .unwrap_or_else(|e| RoundResult::Failed {
            reason: e.to_string(),
            work: None,
        });

    record_completion(frames, result)
}

/// One round, from empty checkout to discarded checkout.
///
/// The agent's work is committed and published *before* success is decided:
/// an unrecorded change would be a lost change, and a failure that produced a
/// diff is exactly the case where the diff is worth reading.
async fn round_result<W: Write + Send + 'static>(
    payload: &JobPayload,
    frames: &FrameWriter<W>,
    scratch_root: &Path,
    timeout: Option<Duration>,
    cancel: CancellationToken,
) -> anyhow::Result<RoundResult> {
    // Set before the clone, so a slow clone leaves less of the shared budget
    // for provisioning rather than a fresh 15 minutes of its own.
    let provisioning_deadline = Instant::now() + PROVISIONING_LIMIT;
    // Unlike `mise` below, the clone may be dropped from outside — timed out
    // or cancelled — because dropping `git` kills its whole process group
    // (`git::run_allowing_failure`).
    let cloning = tokio::time::timeout(
        PROVISIONING_LIMIT,
        workspace::create(
            &payload.remote_url,
            &payload.start,
            &payload.branch,
            &payload.seed_from,
            &payload.copy,
            scratch_root,
            // A container runner always sends the token, having no other
            // credentials to offer; without one, git uses whatever this
            // environment already has.
            std::env::var_os(GIT_TOKEN_VAR)
                .is_some()
                .then_some(git::TOKEN_CREDENTIAL_HELPER),
        ),
    );
    let ws = tokio::select! {
        cloned = cloning => cloned.map_err(|_| {
            anyhow::anyhow!(
                "provisioning timed out after {}",
                humantime::format_duration(PROVISIONING_LIMIT)
            )
        })??,
        () = cancel.cancelled() => anyhow::bail!("cancelled"),
    };

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

    // Bound before the literal: both borrow `ws`, which `discard` consumes.
    let preserved = agent_work_on_branch(payload, &ws).await;
    let verdict = match agent_failure {
        Some(_) => VerifyVerdict::NoObjection,
        None => {
            verify_verdict(
                payload.verify.as_deref(),
                ws.path(),
                frames,
                timeout,
                cancel,
            )
            .await
        }
    };
    // Removing the checkout is housekeeping: failing at it neither undoes a
    // pushed branch nor says anything about the work, so it is reported
    // rather than allowed to replace what the round did.
    if let Err(e) = workspace::discard(ws) {
        frames.append_output(&format!("could not remove the scratch checkout: {e}"))?;
    }
    let work = preserved?;

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
    payload: &JobPayload,
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
enum VerifyVerdict {
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

/// What `verify` made of what the agent left, run in the job's own checkout
/// after the commit, so it judges exactly the tree the branch carries.
///
/// The outcome is matched here rather than flattened through
/// `ShellOutcome::failure_reason`, which cannot tell `exit 1` from a kill.
async fn verify_verdict<W: Write + Send + 'static>(
    verify: Option<&str>,
    workspace: &Path,
    frames: &FrameWriter<W>,
    timeout: Option<Duration>,
    cancel: CancellationToken,
) -> VerifyVerdict {
    let Some(command) = verify else {
        return VerifyVerdict::NoObjection;
    };

    match run_shell(command, workspace, frames, timeout, cancel).await {
        Ok(ShellOutcome::Exited(0)) => VerifyVerdict::NoObjection,
        Ok(ShellOutcome::Exited(code)) => VerifyVerdict::Rejected {
            reason: format!("exit {code}"),
        },
        Ok(ShellOutcome::TimedOut) => VerifyVerdict::Interrupted {
            reason: "verify timed out".to_string(),
        },
        Ok(ShellOutcome::Cancelled) => VerifyVerdict::Interrupted {
            reason: "verify cancelled".to_string(),
        },
        // A verdict rather than an error: the branch is already on the
        // remote, and an error would take the round's record of it down too.
        Err(e) => VerifyVerdict::Interrupted {
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

fn record_completion<W: Write>(
    frames: &FrameWriter<W>,
    result: RoundResult,
) -> anyhow::Result<JobOutcome> {
    let (events, outcome) = events_for_completion(result);

    events
        .into_iter()
        .try_for_each(|kind| frames.append_event(kind).map(|_| ()))?;

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

#[cfg(test)]
mod provisioning_tests {
    //! `provision_toolchain` is private, and the deadline it takes
    //! cannot be exercised through the public `job-exec` surface without
    //! either waiting out the real 15-minute `PROVISIONING_LIMIT` or making
    //! it configurable — both ruled out. This calls the internal function
    //! directly instead, which needs no subprocess: a deadline that has
    //! already passed must fail the very first step without ever running
    //! `mise`, so no fake binary or `PATH` juggling is needed either.

    use super::*;
    use crate::git::PinnedRef;
    use std::path::PathBuf;

    fn payload_asking_for_provisioning() -> JobPayload {
        JobPayload {
            job_id: 1,
            round: 1,
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
            copy: Vec::new(),
            seed_from: PathBuf::from("."),
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
