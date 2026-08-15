use crate::config::{Graph, OnFailure, Task, TaskKind, parse_duration};
use crate::dag::Dag;
use crate::event::{EventKind, EventLog, RunStatus};
use crate::exec::{ShellOutcome, run_shell};
use crate::paths::RunPaths;
use crate::state::{NodeState, RunState, TaskMap, task_map};
use std::collections::BTreeSet;
use std::path::PathBuf;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

pub const AGENT_UNSUPPORTED: &str = "agent nodes are not supported yet (M2)";

#[derive(Debug, Clone)]
pub struct RunOpts {
    pub jobs: usize,
    pub cwd: PathBuf,
    pub cancel: CancellationToken,
}

/// What a finished node reports back to the loop.
struct NodeCompletion {
    node: String,
    resource: Option<String>,
    outcome: Result<ShellOutcome, String>,
}

impl NodeCompletion {
    fn failure_reason(&self) -> Option<String> {
        match &self.outcome {
            Err(spawn_error) => Some(spawn_error.clone()),
            Ok(outcome) => outcome.failure_reason(),
        }
    }
}

/// Nodes that may start right now: dependency-ready, within the job cap, and
/// holding no resource another node is already using.
///
/// Resources are claimed as the selection is built, so two ready nodes sharing
/// a label cannot both be launched in the same pass.
fn nodes_clear_to_launch(
    state: &RunState,
    dag: &Dag,
    tasks: &TaskMap,
    resources_in_use: &BTreeSet<String>,
    free_slots: usize,
) -> Vec<String> {
    state
        .ready(dag, tasks)
        .into_iter()
        .scan(resources_in_use.clone(), |claimed, id| {
            let resource = tasks.get(id.as_str()).and_then(|t| t.resource.clone());
            Some(match resource {
                Some(r) if claimed.contains(&r) => None,
                Some(r) => {
                    claimed.insert(r);
                    Some(id)
                }
                None => Some(id),
            })
        })
        .flatten()
        .take(free_slots)
        .collect()
}

fn wall_clock_limit(task: &Task) -> anyhow::Result<Option<std::time::Duration>> {
    task.max_duration.as_deref().map(parse_duration).transpose()
}

pub async fn execute(
    graph: &Graph,
    dag: &Dag,
    paths: &RunPaths,
    log: &mut EventLog,
    state: &mut RunState,
    opts: &RunOpts,
) -> anyhow::Result<RunStatus> {
    let tasks = task_map(&graph.tasks);
    let mut resources_in_use: BTreeSet<String> = BTreeSet::new();
    let mut in_flight: JoinSet<NodeCompletion> = JoinSet::new();
    let mut aborting = false;

    let ev = log.append(EventKind::RunStarted {
        run_id: paths.id,
        jobs: opts.jobs,
    })?;
    state.apply(&ev.kind);

    // A state machine over time: launch what fits, await one completion,
    // re-derive readiness. Not expressible as an iterator chain.
    loop {
        if !aborting {
            let free_slots = opts.jobs.saturating_sub(in_flight.len());
            for id in nodes_clear_to_launch(state, dag, &tasks, &resources_in_use, free_slots) {
                let Some(task) = tasks.get(id.as_str()).copied() else {
                    continue;
                };
                let timeout = wall_clock_limit(task)?;

                let ev = log.append(EventKind::NodeStarted {
                    node: id.clone(),
                    round: 1,
                })?;
                state.apply(&ev.kind);
                if let Some(r) = &task.resource {
                    resources_in_use.insert(r.clone());
                }

                let resource = task.resource.clone();
                let log_path = paths.log(&id);
                let cwd = opts.cwd.clone();
                let cancel = opts.cancel.clone();

                match task.kind {
                    TaskKind::Agent => {
                        in_flight.spawn(async move {
                            NodeCompletion {
                                node: id,
                                resource,
                                outcome: Err(AGENT_UNSUPPORTED.to_string()),
                            }
                        });
                    }
                    TaskKind::Shell => {
                        let cmd = task.run.clone().unwrap_or_default();
                        in_flight.spawn(async move {
                            let outcome = run_shell(&cmd, &cwd, &log_path, timeout, cancel).await;
                            NodeCompletion {
                                node: id,
                                resource,
                                outcome: outcome.map_err(|e| e.to_string()),
                            }
                        });
                    }
                }
            }
        }

        let Some(joined) = in_flight.join_next().await else {
            break;
        };
        let completion = joined?;

        if let Some(r) = &completion.resource {
            resources_in_use.remove(r);
        }

        match completion.failure_reason() {
            None => {
                let ev = log.append(EventKind::NodeFinished {
                    node: completion.node.clone(),
                    exit_code: 0,
                })?;
                state.apply(&ev.kind);
            }
            Some(reason) => {
                let ev = log.append(EventKind::NodeFailed {
                    node: completion.node.clone(),
                    reason,
                })?;
                state.apply(&ev.kind);

                match tasks
                    .get(completion.node.as_str())
                    .map(|t| t.on_failure)
                    .unwrap_or_default()
                {
                    OnFailure::Continue => {}
                    OnFailure::Skip => mark_pending_as_skipped(
                        log,
                        state,
                        dag.descendants(&completion.node),
                        &format!("needs {}", completion.node),
                    )?,
                    OnFailure::Abort => {
                        aborting = true;
                        opts.cancel.cancel();
                    }
                }
            }
        }
    }

    if aborting {
        // Record what never got a chance rather than silently dropping it.
        mark_pending_as_skipped(log, state, dag.ids().iter().cloned(), "run aborted")?;
    }

    let counts = state.counts();
    let status = match (aborting, counts.failed + counts.skipped) {
        (true, _) => RunStatus::Aborted,
        (false, 0) => RunStatus::Ok,
        (false, _) => RunStatus::Partial,
    };

    let ev = log.append(EventKind::RunFinished { status })?;
    state.apply(&ev.kind);
    Ok(status)
}

/// Record the still-pending members of `candidates` as skipped. Nodes that
/// already started are left alone — one running under a `continue` parent must
/// not be retroactively skipped.
fn mark_pending_as_skipped(
    log: &mut EventLog,
    state: &mut RunState,
    candidates: impl IntoIterator<Item = String>,
    because: &str,
) -> anyhow::Result<()> {
    let pending: Vec<String> = candidates
        .into_iter()
        .filter(|id| state.state(id) == NodeState::Pending)
        .collect();

    pending.into_iter().try_for_each(|id| {
        let ev = log.append(EventKind::NodeSkipped {
            node: id,
            because: because.to_string(),
        })?;
        state.apply(&ev.kind);
        anyhow::Ok(())
    })
}
