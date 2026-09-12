use crate::config::{Graph, Task, parse_duration};
use crate::event::{EventKind, EventLog, RunStatus};
use crate::exec::{ShellOutcome, run_command};
use crate::git;
use crate::paths::{self, RunPaths};
use crate::provider::{CommandSpec, render_command};
use crate::state::{NodeState, RunState, task_map};
use crate::workspace::{self, node_branch_name};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct RunOpts {
    pub jobs: usize,
    pub cwd: PathBuf,
    pub cancel: CancellationToken,
    /// The repository worktrees are created from. Every task is an agent
    /// task, so `None` here means every node fails rather than runs.
    pub repo: Option<PathBuf>,
    /// Where `copy` paths resolve from — the CLI's working directory.
    pub seed_from: PathBuf,
    /// The remote node branches are published to. A repository that has no
    /// remote by this name keeps its branches locally instead.
    pub remote: String,
}

/// What an agent left behind, recorded on its own branch.
///
/// This is the whole durable output of a job. The checkout it was produced in
/// is gone by the time this exists.
#[derive(Debug)]
struct AgentWork {
    branch: String,
    sha: String,
    stat: git::DiffStat,
    /// The remote the branch reached, or `None` for a repository with none.
    pushed_to: Option<String>,
}

/// What a finished node reports back to the loop. Events are written by the
/// loop, not the task, so their order in the log is the order things settled.
#[derive(Debug)]
enum NodeResult {
    /// An agent that correctly decided nothing needed doing.
    Succeeded,
    /// An agent left work, which was committed and published. The branch is
    /// the whole durable output of a job.
    Committed { work: AgentWork },
    /// `work` carries whatever the agent had produced before it failed. That
    /// work is preserved on the node's branch.
    Failed {
        reason: String,
        work: Option<AgentWork>,
    },
}

impl NodeResult {
    /// A command's outcome, where an unspawnable program and a non-zero exit
    /// both mean the node failed — but with different reasons.
    fn from_command(outcome: anyhow::Result<ShellOutcome>) -> Self {
        match outcome.map(|o| o.failure_reason()) {
            Err(unspawnable) => NodeResult::Failed {
                reason: unspawnable.to_string(),
                work: None,
            },
            Ok(Some(reason)) => NodeResult::Failed { reason, work: None },
            Ok(None) => NodeResult::Succeeded,
        }
    }

    /// The reason this node failed, if it did.
    fn failure_reason(self) -> Option<String> {
        match self {
            NodeResult::Failed { reason, .. } => Some(reason),
            _ => None,
        }
    }
}

/// What a finished node reports back to the loop.
struct NodeCompletion {
    node: String,
    result: NodeResult,
}

/// Everything an agent node needs, resolved before the node is spawned so a
/// config mistake becomes a failed node with a clear reason rather than a
/// panic inside a task.
struct AgentNodePlan {
    repo: PathBuf,
    base_sha: String,
    workspace_path: PathBuf,
    branch: String,
    seed_from: PathBuf,
    copy_paths: Vec<String>,
    command: CommandSpec,
    commit_message: String,
    remote: String,
    /// A first attempt cuts a fresh branch off the base; a revise round
    /// continues the node's own branch, so the agent starts from its prior
    /// work.
    continues_branch: bool,
}

/// Tasks not yet started, up to the job cap.
fn tasks_clear_to_launch(state: &RunState, graph: &Graph, free_slots: usize) -> Vec<String> {
    graph
        .tasks
        .iter()
        .filter(|t| state.state(&t.id) == NodeState::Pending)
        .map(|t| t.id.clone())
        .take(free_slots)
        .collect()
}

fn wall_clock_limit(task: &Task) -> anyhow::Result<Option<Duration>> {
    task.max_duration.as_deref().map(parse_duration).transpose()
}

/// Files a node's workspace is seeded with: the graph-wide list plus the
/// task's own, first occurrence winning.
fn declared_copy_paths(graph: &Graph, task: &Task) -> Vec<String> {
    graph
        .workspace
        .copy
        .iter()
        .chain(&task.copy)
        .fold(Vec::new(), |mut acc, path| {
            if !acc.contains(path) {
                acc.push(path.clone());
            }
            acc
        })
}

/// A commit subject a human can scan in `git log`: the node, then the first
/// non-blank line of what it was asked to do.
fn commit_message(node: &str, prompt: &str) -> String {
    match prompt.lines().find(|line| !line.trim().is_empty()) {
        Some(first) => format!("{node}: {}", first.trim()),
        None => format!("{node}: agent work"),
    }
}

/// Resolve an agent node against its graph, or say why it cannot run.
///
/// The error is a message rather than a typed value because its only
/// destination is the node's `NodeFailed` reason.
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

fn agent_node_plan(
    graph: &Graph,
    task: &Task,
    paths: &RunPaths,
    opts: &RunOpts,
    repo_and_base: Option<(&Path, &str)>,
    revision: Option<&str>,
) -> Result<AgentNodePlan, String> {
    let (repo, base_sha) = repo_and_base.ok_or(
        "agent nodes need a git repository to create worktrees from, and this run has none",
    )?;

    let provider_name = task
        .provider
        .as_deref()
        .ok_or_else(|| format!("agent task '{}' names no provider", task.id))?;
    let provider = graph
        .providers
        .get(provider_name)
        .ok_or_else(|| format!("undefined provider '{provider_name}'"))?;

    let workspace_path = paths
        .node_worktree(repo, &task.id)
        .ok_or("HOME is unset, so assembly-line has nowhere to put worktrees")?;

    let original = task.prompt.clone().unwrap_or_default();
    // The commit subject keeps using the original's first line, so `git log`
    // reads the same across rounds.
    let prompt = revision.map_or_else(
        || original.clone(),
        |feedback| revised_prompt(&original, feedback),
    );

    Ok(AgentNodePlan {
        repo: repo.to_path_buf(),
        base_sha: base_sha.to_string(),
        workspace_path,
        branch: node_branch_name(paths.id, &task.id),
        seed_from: opts.seed_from.clone(),
        copy_paths: declared_copy_paths(graph, task),
        command: render_command(provider, &prompt),
        commit_message: commit_message(&task.id, &original),
        remote: opts.remote.clone(),
        continues_branch: revision.is_some(),
    })
}

/// One revise round: which node, what to change about it, and which round
/// this is.
#[derive(Debug, Clone, Copy)]
pub struct Revision<'a> {
    pub node: &'a str,
    pub feedback: &'a str,
    pub round: u32,
}

/// Run one node again, based at its own branch, with feedback folded into its
/// prompt. Returns whether the round failed.
///
/// A revise is a *new job*, not a resumption. Nothing was kept from the last
/// round except the branch — which is exactly what the agent needs, because
/// its prior work arrives as files on disk.
///
/// # Errors
///
/// Returns an error if the node is not a task the graph declares, if the run
/// has no repository, or if the event log cannot be appended to. A round that
/// runs and fails is not an error — that is the returned flag.
pub async fn revise_node(
    graph: &Graph,
    paths: &RunPaths,
    log: &mut EventLog,
    state: &mut RunState,
    opts: &RunOpts,
    revision: &Revision<'_>,
) -> anyhow::Result<bool> {
    let Revision {
        node,
        feedback,
        round,
    } = *revision;

    let task = graph
        .tasks
        .iter()
        .find(|t| t.id == node)
        .ok_or_else(|| anyhow::anyhow!("the graph has no task '{node}'"))?;
    let repo = opts
        .repo
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("revising needs a git repository"))?;
    paths::record_repository_for_worktrees(repo)?;
    let branch = node_branch_name(paths.id, node);
    let base_sha = git::branch_tip(repo, &branch).await?;

    let plan = agent_node_plan(
        graph,
        task,
        paths,
        opts,
        Some((repo, base_sha.as_str())),
        Some(feedback),
    )
    .map_err(|reason| anyhow::anyhow!(reason))?;

    let ev = log.append(EventKind::NodeStarted {
        node: node.to_string(),
        round,
    })?;
    state.apply(&ev.kind);

    let result = agent_node_result(
        &plan,
        &paths.log(node),
        wall_clock_limit(task)?,
        opts.cancel.clone(),
    )
    .await
    .unwrap_or_else(|e| NodeResult::Failed {
        reason: e.to_string(),
        work: None,
    });

    record_completion(log, state, node, result)
}

/// Run one agent node end to end: sandbox, agent, commit, publish.
///
/// The job is stateless — its checkout is scratch and is discarded whatever
/// happened, including on failure. What survives is the node's branch, which
/// is why the agent's work is committed and published *before* success is
/// decided: an unrecorded change would be a lost change.
async fn agent_node_result(
    plan: &AgentNodePlan,
    log_path: &Path,
    timeout: Option<Duration>,
    cancel: CancellationToken,
) -> anyhow::Result<NodeResult> {
    // A first attempt branches from the repository's own base; a revise round
    // branches from its own last round, so its diff reports what *this* round
    // changed.
    let start = match plan.continues_branch {
        true => workspace::StartPoint::ContinueBranch,
        false => workspace::StartPoint::FreshBranch(&plan.base_sha),
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

    let ran = NodeResult::from_command(
        run_command(&plan.command, &ws.path, log_path, timeout, cancel).await,
    );

    // Preserve first, judge after. A failed agent that got halfway is exactly
    // the case where the diff is worth reading.
    let work = match workspace::commit(&ws, &plan.commit_message).await? {
        None => None,
        Some(sha) => Some(AgentWork {
            branch: ws.branch.clone(),
            sha,
            stat: git::diff_stat_against(&ws.path, &plan.base_sha).await?,
            pushed_to: workspace::publish(&plan.repo, &ws, &plan.remote).await?,
        }),
    };

    workspace::discard(&plan.repo, &ws).await?;

    if let Some(reason) = ran.failure_reason() {
        return Ok(NodeResult::Failed { reason, work });
    }

    Ok(match work {
        None => NodeResult::Succeeded,
        Some(work) => NodeResult::Committed { work },
    })
}

/// Confirm the repository has something to branch from, and return its
/// current `HEAD` — the base every fresh node branch in this run is cut from.
///
/// # Errors
///
/// Returns an error if the repository has no commits, or if the marker
/// naming it for `gc` cannot be written.
async fn base_for_fresh_branches(repo: &Path) -> anyhow::Result<String> {
    anyhow::ensure!(
        git::has_commits(repo).await?,
        "the repository has no commits, so an agent node has nothing to branch from"
    );

    // Records which repository these worktrees belong to, so `gc` can collect
    // them without being run from the repository itself.
    paths::record_repository_for_worktrees(repo)?;

    git::head_sha(repo).await
}

/// Write every event a completion implies, in order, and report whether the
/// node ended up failed.
///
/// All log writes stay on the loop's own task, so the log's order is the order
/// in which things actually settled.
fn record_completion(
    log: &mut EventLog,
    state: &mut RunState,
    node: &str,
    result: NodeResult,
) -> anyhow::Result<bool> {
    let (events, node_failed) = events_for_completion(node, result);

    events.into_iter().try_for_each(|kind| {
        let ev = log.append(kind)?;
        state.apply(&ev.kind);
        anyhow::Ok(())
    })?;

    Ok(node_failed)
}

/// Recording what an agent left: the commit, then where its branch went.
///
/// Empty for an agent that correctly changed nothing.
fn work_recorded(node: &str, work: Option<&AgentWork>) -> Vec<EventKind> {
    work.map(|w| {
        vec![
            EventKind::NodeCommitted {
                node: node.to_string(),
                sha: w.sha.clone(),
                files: w.stat.files,
                insertions: w.stat.insertions,
                deletions: w.stat.deletions,
            },
            EventKind::NodeBranchPublished {
                node: node.to_string(),
                branch: w.branch.clone(),
                pushed_to: w.pushed_to.clone(),
            },
        ]
    })
    .unwrap_or_default()
}

/// The events a completion implies, in the order things settled, paired with
/// whether the node ended up failed.
///
/// Pure, so the ordering that makes replay correct can be asserted without
/// running anything. Work is always recorded first: a failure that produced a
/// diff still produced a diff.
fn events_for_completion(node: &str, result: NodeResult) -> (Vec<EventKind>, bool) {
    let finished = EventKind::NodeFinished {
        node: node.to_string(),
        exit_code: 0,
    };
    let failed = |reason| EventKind::NodeFailed {
        node: node.to_string(),
        reason,
    };

    match result {
        NodeResult::Succeeded => (vec![finished], false),
        NodeResult::Failed { reason, work } => (
            work_recorded(node, work.as_ref())
                .into_iter()
                .chain([failed(reason)])
                .collect(),
            true,
        ),
        NodeResult::Committed { work } => (
            work_recorded(node, Some(&work))
                .into_iter()
                .chain([finished])
                .collect(),
            false,
        ),
    }
}

/// Drive the graph to completion, recording every transition to the log.
///
/// # Errors
///
/// Returns an error only if the run cannot be *administered* — the event log
/// cannot be appended to, a task's `max_duration` is unparseable, the
/// repository has no commits for a fresh node branch to start from, or a
/// spawned task panicked. A node that fails, times out, or is cancelled is not
/// an error: that is reflected in the returned [`RunStatus`], because a
/// partial run is a normal outcome.
///
/// # Panics
///
/// Does not panic. A panic inside a node's task is surfaced as an error.
pub async fn execute(
    graph: &Graph,
    paths: &RunPaths,
    log: &mut EventLog,
    state: &mut RunState,
    opts: &RunOpts,
) -> anyhow::Result<RunStatus> {
    let tasks = task_map(&graph.tasks);
    let mut in_flight: JoinSet<NodeCompletion> = JoinSet::new();

    let ev = log.append(EventKind::RunStarted {
        run_id: paths.id,
        jobs: opts.jobs,
    })?;
    state.apply(&ev.kind);

    let repo_and_base = match &opts.repo {
        None => None,
        Some(repo) => Some((repo.clone(), base_for_fresh_branches(repo).await?)),
    };

    // A state machine over time: launch what fits, await one completion,
    // re-derive readiness. Not expressible as an iterator chain.
    loop {
        let free_slots = opts.jobs.saturating_sub(in_flight.len());
        for id in tasks_clear_to_launch(state, graph, free_slots) {
            let Some(task) = tasks.get(id.as_str()).copied() else {
                continue;
            };
            let timeout = wall_clock_limit(task)?;

            let ev = log.append(EventKind::NodeStarted {
                node: id.clone(),
                round: 1,
            })?;
            state.apply(&ev.kind);

            let log_path = paths.log(&id);
            let cancel = opts.cancel.clone();

            let repo_and_base = repo_and_base
                .as_ref()
                .map(|(repo, base_sha)| (repo.as_path(), base_sha.as_str()));
            match agent_node_plan(graph, task, paths, opts, repo_and_base, None) {
                Err(reason) => {
                    in_flight.spawn(async move {
                        NodeCompletion {
                            node: id,
                            // A node that never ran left nothing to preserve.
                            result: NodeResult::Failed { reason, work: None },
                        }
                    });
                }
                Ok(plan) => {
                    in_flight.spawn(async move {
                        let result = agent_node_result(&plan, &log_path, timeout, cancel).await;
                        NodeCompletion {
                            node: id,
                            // The job could not be administered at all — no
                            // sandbox, or git refused. Any partial work is
                            // described by the error, not by a branch we can
                            // name.
                            result: result.unwrap_or_else(|e| NodeResult::Failed {
                                reason: e.to_string(),
                                work: None,
                            }),
                        }
                    });
                }
            }
        }

        let Some(joined) = in_flight.join_next().await else {
            break;
        };
        let completion = joined?;
        record_completion(log, state, &completion.node, completion.result)?;
    }

    let status = match state.counts().failed {
        0 => RunStatus::Ok,
        _ => RunStatus::Partial,
    };

    let ev = log.append(EventKind::RunFinished { status })?;
    state.apply(&ev.kind);
    Ok(status)
}
