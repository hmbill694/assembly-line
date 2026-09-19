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
use crate::state::JobState;
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
enum JobResult {
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

impl JobResult {
    /// A command's outcome, where an unspawnable program and a non-zero exit
    /// both mean the job failed — but with different reasons.
    fn from_command(outcome: anyhow::Result<ShellOutcome>) -> Self {
        match outcome.map(|o| o.failure_reason()) {
            Err(unspawnable) => JobResult::Failed {
                reason: unspawnable.to_string(),
                work: None,
            },
            Ok(Some(reason)) => JobResult::Failed { reason, work: None },
            Ok(None) => JobResult::Succeeded,
        }
    }

    /// The reason this round failed, if the agent itself is what failed it.
    fn failure_reason(self) -> Option<String> {
        match self {
            JobResult::Failed { reason, .. } => Some(reason),
            JobResult::Succeeded
            | JobResult::Committed { .. }
            | JobResult::VerifyRejected { .. } => None,
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
/// Returns whether the job failed.
///
/// # Errors
///
/// Returns an error only if the job cannot be *administered* — the event log
/// cannot be appended to, `max_duration` is unparseable, the repository does
/// not declare the provider, or git refused to make a checkout. An agent that
/// runs and fails is not an error: that is the returned flag.
pub async fn run_job(
    config: &RepoConfig,
    spec: &JobSpec<'_>,
    paths: &JobPaths,
    log: &mut EventLog,
    state: &mut JobState,
    opts: &RunOpts,
) -> anyhow::Result<bool> {
    let timeout = wall_clock_limit(config)?;
    let branch = job_branch_name(paths.id);
    let start_sha = start_commit_for_round(&opts.repo, &branch, spec).await?;
    let plan = job_plan(config, spec, paths, opts, &start_sha)?;

    let ev = log.append(EventKind::JobStarted { round: spec.round })?;
    state.apply(&ev.kind);

    let result = round_result(&plan, &paths.log(), timeout, opts.cancel.clone())
        .await
        // The round could not be administered at all — no sandbox, or git
        // refused. Any partial work is described by the error, not by a branch
        // we can name.
        .unwrap_or_else(|e| JobResult::Failed {
            reason: e.to_string(),
            work: None,
        });

    record_completion(log, state, result)
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
    state: &mut JobState,
    opts: &RunOpts,
) -> anyhow::Result<bool> {
    let prompt = revised_prompt(&meta.prompt, revision.feedback);
    let spec = JobSpec {
        prompt: &prompt,
        provider: &meta.provider,
        base_ref: &meta.base_ref,
        round: revision.round,
    };
    run_job(config, &spec, paths, log, state, opts).await
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
) -> anyhow::Result<JobResult> {
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

    let ran = JobResult::from_command(
        run_command(&plan.command, &ws.path, log_path, timeout, cancel.clone()).await,
    );
    // Taken once, up front: it decides both whether `verify` is worth running
    // and how the round ends, and an agent failure wins over either answer.
    let agent_failure = ran.failure_reason();

    // Preserve first, judge after — and discard the checkout whichever way
    // preserving went. The checkout is scratch holding whatever was seeded
    // into it, so leaving it for `gc` because a git call failed is not an
    // option; both outcomes are held and reported after it is gone.
    //
    // `verify` runs here too, in this same checkout, after the commit: it
    // judges exactly the tree the branch now carries, and what it gates is
    // delivery, never the branch's survival. A round whose agent already
    // failed skips it: there is nothing to judge but an abandoned tree, and
    // the answer could not change the outcome — so at least the failure path
    // does not also pay for a `verify` run. It is not a general budget,
    // though: `verify` is a user-authored shell line that gets its own full
    // `max_duration`, same as the agent, so a round that succeeds can still
    // take up to 2x `max_duration` end to end. A shared remaining-budget is a
    // later milestone's problem.
    let preserved = agent_work_on_branch(plan, &ws).await;
    let verdict = match agent_failure {
        Some(_) => Ok(VerifyVerdict::NoObjection),
        None => verify_verdict(plan.verify.as_deref(), &ws.path, log_path, timeout, cancel).await,
    };
    let discarded = workspace::discard(&plan.repo, &ws).await;
    let work = preserved?;
    let verdict = verdict?;
    discarded?;

    Ok(match (agent_failure, verdict) {
        // The two failures that are not judgements about the work: an agent
        // failure, which is the earlier and more fundamental one and is why
        // `verify` was never asked; and a `verify` killed before it could
        // judge anything, which fails the round as itself rather than as a
        // rejection.
        (Some(reason), _) | (None, VerifyVerdict::Interrupted { reason }) => {
            JobResult::Failed { reason, work }
        }
        (None, VerifyVerdict::Rejected { reason }) => JobResult::VerifyRejected { reason, work },
        (None, VerifyVerdict::NoObjection) => match work {
            None => JobResult::Succeeded,
            Some(work) => JobResult::Committed { work },
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

/// Write every event a completion implies, in order, and report whether the
/// job ended up failed.
fn record_completion(
    log: &mut EventLog,
    state: &mut JobState,
    result: JobResult,
) -> anyhow::Result<bool> {
    let (events, job_failed) = events_for_completion(result);

    events.into_iter().try_for_each(|kind| {
        let ev = log.append(kind)?;
        state.apply(&ev.kind);
        anyhow::Ok(())
    })?;

    Ok(job_failed)
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
/// whether the job ended up failed.
///
/// Pure, so the ordering that makes replay correct can be asserted without
/// running anything. Work is always recorded first: a failure that produced a
/// diff still produced a diff.
fn events_for_completion(result: JobResult) -> (Vec<EventKind>, bool) {
    let finished = EventKind::JobFinished { exit_code: 0 };

    match result {
        JobResult::Succeeded => (vec![finished], false),
        JobResult::Failed { reason, work } => (
            work_recorded(work.as_ref())
                .into_iter()
                .chain([EventKind::JobFailed { reason }])
                .collect(),
            true,
        ),
        JobResult::VerifyRejected { reason, work } => (
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
            true,
        ),
        JobResult::Committed { work } => (
            work_recorded(Some(&work))
                .into_iter()
                .chain([finished])
                .collect(),
            false,
        ),
    }
}
