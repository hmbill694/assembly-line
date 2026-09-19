//! Running one job: a repository, a ref and a prompt.
//!
//! A job is stateless. Its checkout is scratch and is discarded whatever
//! happened, including on failure; its branch is the whole durable output,
//! which is why work is committed and published *before* success is decided.

use crate::config::{ConfigError, RepoConfig, parse_duration};
use crate::event::{EventKind, EventLog};
use crate::exec::{ShellOutcome, run_command, run_shell};
use crate::git;
use crate::paths::{self, JobMeta, JobPaths};
use crate::provider::{CommandSpec, render_command};
use crate::workspace::{self, JobWorkspace, job_branch_name};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// The parts of running a job that come from the machine rather than from the
/// repository's own configuration.
#[derive(Debug, Clone)]
pub struct RunOpts {
    pub cancel: CancellationToken,
    /// The repository the checkout is created from.
    pub repo: PathBuf,
    /// Where `copy` paths resolve from.
    pub seed_from: PathBuf,
    /// The remote the job's branch is published to. A repository that has no
    /// remote by this name keeps its branch locally instead.
    pub remote: String,
}

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

/// One job: which prompt, run by which provider, against which ref.
#[derive(Debug, Clone, Copy)]
pub struct JobSpec<'a> {
    pub prompt: &'a str,
    pub provider: &'a str,
    pub base_ref: &'a str,
    /// 1 for a first attempt; higher for a revise round, which continues the
    /// job's own branch.
    pub round: u32,
}

/// What an agent left behind, recorded on the job's branch.
///
/// This is the whole durable output of a job. The checkout it was produced in
/// is gone by the time this exists.
#[derive(Debug)]
struct AgentWork {
    branch: String,
    sha: String,
    stat: git::DiffStat,
    /// The remote the branch reached, or `None` for a repository with no such
    /// remote — or one that refused the push.
    pushed_to: Option<String>,
}

/// How a round ended.
#[derive(Debug)]
enum RoundResult {
    /// An agent that correctly decided nothing needed doing.
    Succeeded,
    /// An agent left work, which was committed and published.
    Committed { work: AgentWork },
    /// `verify` rejected an otherwise-successful round. The work — if there
    /// was any — is preserved regardless: a rejection is exactly the case
    /// where the diff is worth reading.
    VerifyRejected {
        reason: String,
        work: Option<AgentWork>,
    },
    /// `work` carries whatever the agent had produced before it failed. That
    /// work is preserved on the job's branch.
    Failed {
        reason: String,
        work: Option<AgentWork>,
    },
}

impl RoundResult {
    /// A command's outcome, where an unspawnable program and a non-zero exit
    /// both mean the job failed — but with different reasons.
    fn from_command(outcome: anyhow::Result<ShellOutcome>) -> Self {
        match outcome.map(|o| o.failure_reason()) {
            Err(unspawnable) => RoundResult::Failed {
                reason: unspawnable.to_string(),
                work: None,
            },
            Ok(Some(reason)) => RoundResult::Failed { reason, work: None },
            Ok(None) => RoundResult::Succeeded,
        }
    }

    /// The reason this round failed, if the agent itself is what failed it.
    fn failure_reason(self) -> Option<String> {
        match self {
            RoundResult::Failed { reason, .. } => Some(reason),
            RoundResult::Succeeded
            | RoundResult::Committed { .. }
            | RoundResult::VerifyRejected { .. } => None,
        }
    }
}

/// Everything a round needs, resolved before the agent is spawned so a config
/// mistake is reported against the command line rather than surfacing as a
/// mysteriously failed job.
#[derive(Debug)]
struct JobPlan {
    repo: PathBuf,
    /// The commit the checkout starts at — the base ref for a first attempt,
    /// the job's own branch tip for a revise round.
    start_sha: String,
    workspace_path: PathBuf,
    branch: String,
    seed_from: PathBuf,
    copy_paths: Vec<String>,
    command: CommandSpec,
    commit_message: String,
    remote: String,
    /// A first attempt cuts a fresh branch off the base; a revise round
    /// continues the job's own branch, so the agent starts from its prior
    /// work.
    continues_branch: bool,
    /// The command that decides whether the round's work is accepted, or
    /// `None` for a repository with no `verify` — which accepts on the
    /// agent's exit code alone.
    verify: Option<String>,
}

/// The cap on one command. Both the agent and `verify` get it in full — see
/// [`RepoConfig::max_duration`].
fn wall_clock_limit(config: &RepoConfig) -> anyhow::Result<Option<Duration>> {
    config
        .max_duration
        .as_deref()
        .map(parse_duration)
        .transpose()
}

/// A commit subject a human can scan in `git log`: the job, then the first
/// non-blank line of what it was asked to do.
fn commit_message(job_id: u64, prompt: &str) -> String {
    match prompt.lines().find(|line| !line.trim().is_empty()) {
        Some(first) => format!("job {job_id}: {}", first.trim()),
        None => format!("job {job_id}: agent work"),
    }
}

/// The prompt a revise round carries: what was originally asked, then what to
/// change about the answer.
///
/// The agent's prior work is already committed in the tree it is about to be
/// dropped into, so the feedback is the only new context it needs. That is
/// what makes revising behave identically across every provider — no session
/// replay, no conversation history.
fn revised_prompt(original: &str, feedback: &str) -> String {
    format!(
        "{original}\n\n---\n\nYour previous attempt is already committed in this \
         working tree. Revise it based on this feedback:\n\n{feedback}\n"
    )
}

/// Resolve a job against the repository's configuration.
///
/// # Errors
///
/// Returns a [`ConfigError`] when the repository does not declare the
/// provider the job asked for, and a plain error when there is nowhere to put
/// a worktree.
fn job_plan(
    config: &RepoConfig,
    spec: &JobSpec<'_>,
    paths: &JobPaths,
    opts: &RunOpts,
    start_sha: &str,
) -> anyhow::Result<JobPlan> {
    let provider = config
        .providers
        .get(spec.provider)
        .ok_or_else(|| ConfigError::UnknownProvider(spec.provider.to_string()))?;

    let workspace_path = paths.worktree(&opts.repo).ok_or_else(|| {
        anyhow::anyhow!("HOME is unset, so assembly-line has nowhere to put worktrees")
    })?;

    Ok(JobPlan {
        repo: opts.repo.clone(),
        start_sha: start_sha.to_string(),
        workspace_path,
        branch: job_branch_name(paths.id),
        seed_from: opts.seed_from.clone(),
        copy_paths: config.copy.clone(),
        command: render_command(provider, spec.prompt),
        commit_message: commit_message(paths.id, spec.prompt),
        remote: opts.remote.clone(),
        continues_branch: spec.round > 1,
        verify: config.verify.clone(),
    })
}

/// Where this round's checkout starts: the ref the job was cut from for a
/// first attempt, the job's own branch tip for a revise round.
///
/// Also records which repository these worktrees belong to, so `gc` can
/// collect them without being run from the repository itself.
async fn start_commit_for_round(
    repo: &Path,
    branch: &str,
    spec: &JobSpec<'_>,
) -> anyhow::Result<String> {
    anyhow::ensure!(
        git::has_commits(repo).await?,
        "the repository has no commits, so a job has nothing to branch from"
    );
    paths::record_repository_for_worktrees(repo)?;

    match spec.round > 1 {
        true => git::branch_tip(repo, branch).await,
        false => git::sha_at_ref(repo, spec.base_ref).await,
    }
}

/// Run one job end to end: scratch checkout, agent, commit, publish, discard.
///
/// # Errors
///
/// Returns an error only if the job cannot be *administered* — the event log
/// cannot be appended to, `max_duration` is unparseable, the repository does
/// not declare the provider, or git refused to make a checkout. An agent that
/// runs and fails is not an error: that is [`JobOutcome::Failed`].
pub async fn run_job(
    config: &RepoConfig,
    spec: &JobSpec<'_>,
    paths: &JobPaths,
    log: &mut EventLog,
    opts: &RunOpts,
) -> anyhow::Result<JobOutcome> {
    let timeout = wall_clock_limit(config)?;
    let branch = job_branch_name(paths.id);
    let start_sha = start_commit_for_round(&opts.repo, &branch, spec).await?;
    let plan = job_plan(config, spec, paths, opts, &start_sha)?;

    log.append(EventKind::JobStarted { round: spec.round })?;

    let result = round_result(&plan, &paths.log(), timeout, opts.cancel.clone())
        .await
        // The round could not be administered at all — no sandbox, or git
        // refused. Any partial work is described by the error, not by a branch
        // we can name.
        .unwrap_or_else(|e| RoundResult::Failed {
            reason: e.to_string(),
            work: None,
        });

    record_completion(log, result)
}

/// One revise round: what to change about the last one, and which round this
/// is.
#[derive(Debug, Clone, Copy)]
pub struct Revision<'a> {
    pub feedback: &'a str,
    pub round: u32,
}

/// Another round on this job's branch, with feedback folded into the prompt.
///
/// A revise is a *new job*, not a resumption. Nothing is kept from the last
/// round except the branch — which is exactly what the agent needs, because
/// its prior work arrives as files on disk.
///
/// # Errors
///
/// See [`run_job`].
pub async fn revise_job(
    config: &RepoConfig,
    meta: &JobMeta,
    revision: &Revision<'_>,
    paths: &JobPaths,
    log: &mut EventLog,
    opts: &RunOpts,
) -> anyhow::Result<JobOutcome> {
    let prompt = revised_prompt(&meta.prompt, revision.feedback);
    let spec = JobSpec {
        prompt: &prompt,
        provider: &meta.provider,
        base_ref: &meta.base_ref,
        round: revision.round,
    };
    run_job(config, &spec, paths, log, opts).await
}

/// What a round settled to, in the order it settled: the agent's work
/// preserved on the branch, the verdict on it, then the scratch checkout's
/// removal.
///
/// Three held `Result`s rather than three `?`s. The checkout is scratch
/// holding whatever was seeded into it, so leaving it behind for `gc` because
/// an earlier git call failed is not an option — and building this as one
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
    plan: &JobPlan,
    log_path: &Path,
    timeout: Option<Duration>,
    cancel: CancellationToken,
) -> anyhow::Result<RoundResult> {
    let start = match plan.continues_branch {
        true => workspace::StartPoint::ContinueBranch,
        false => workspace::StartPoint::FreshBranch(&plan.start_sha),
    };

    let ws = workspace::create(
        &plan.repo,
        &plan.workspace_path,
        &plan.branch,
        start,
        &plan.seed_from,
        &plan.copy_paths,
    )
    .await?;

    let ran = RoundResult::from_command(
        run_command(&plan.command, &ws.path, log_path, timeout, cancel.clone()).await,
    );
    // Taken once, up front: it decides both whether `verify` is worth running
    // and how the round ends, and an agent failure wins over either answer.
    let agent_failure = ran.failure_reason();

    let settled = RoundSettlement {
        preserved: agent_work_on_branch(plan, &ws).await,
        verdict: match agent_failure {
            Some(_) => Ok(VerifyVerdict::NoObjection),
            None => {
                verify_verdict(plan.verify.as_deref(), &ws.path, log_path, timeout, cancel).await
            }
        },
        discarded: workspace::discard(&plan.repo, &ws).await,
    };
    let (work, verdict) = settled.work_and_verdict()?;

    Ok(match (agent_failure, verdict) {
        // The two failures that are not judgements about the work: an agent
        // failure, which is the earlier and more fundamental one and is why
        // `verify` was never asked; and a `verify` killed before it could
        // judge anything, which fails the round as itself rather than as a
        // rejection.
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

/// What `verify` made of what the agent left. A repository with no `verify`
/// raises no objection, so the round succeeds on the agent's exit code alone.
///
/// Runs in the job's own checkout, after the commit, so it judges exactly the
/// tree the branch carries.
///
/// The outcome is matched here rather than flattened through
/// `ShellOutcome::failure_reason`, which cannot tell `exit 1` from a kill: a
/// rejection is a verdict about the work, and an interruption is the absence
/// of one.
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

/// What the agent left on the job's branch, or `None` when it changed nothing.
async fn agent_work_on_branch(
    plan: &JobPlan,
    ws: &JobWorkspace,
) -> anyhow::Result<Option<AgentWork>> {
    let Some(sha) = workspace::commit(ws, &plan.commit_message).await? else {
        return Ok(None);
    };

    Ok(Some(AgentWork {
        branch: ws.branch.clone(),
        sha,
        stat: git::diff_stat_against(&ws.path, &plan.start_sha).await?,
        pushed_to: remote_the_branch_reached(plan, ws).await,
    }))
}

/// The remote the branch reached, or `None` for a repository that has no such
/// remote *and* for a push the remote refused.
///
/// Neither is worth losing the record over. The branch is a local ref holding
/// the agent's work either way, and the branch is the job's whole durable
/// output — dropping `JobBranchPublished` because a remote was unreachable
/// would erase the only trace of the one thing a job is for.
async fn remote_the_branch_reached(plan: &JobPlan, ws: &JobWorkspace) -> Option<String> {
    match workspace::publish(&plan.repo, ws, &plan.remote).await {
        Ok(reached) => reached,
        Err(e) => {
            tracing::warn!(
                "'{}' branch stays local: publishing to '{}' failed: {e}",
                ws.branch,
                plan.remote
            );
            None
        }
    }
}

fn record_completion(log: &mut EventLog, result: RoundResult) -> anyhow::Result<JobOutcome> {
    let (events, outcome) = events_for_completion(result);

    events
        .into_iter()
        .try_for_each(|kind| log.append(kind).map(|_| ()))?;

    Ok(outcome)
}

/// Recording what an agent left: the commit, then where its branch went.
///
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
                pushed_to: w.pushed_to.clone(),
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
