use crate::config::{OnFailure, Task};
use crate::dag::Dag;
use crate::event::{Event, EventKind, RunStatus};
use std::collections::BTreeMap;

pub type TaskMap<'a> = BTreeMap<&'a str, &'a Task>;

pub fn task_map(tasks: &[Task]) -> TaskMap<'_> {
    tasks.iter().map(|t| (t.id.as_str(), t)).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeState {
    Pending,
    Running,
    Done,
    Failed,
    Skipped,
}

/// Counts of terminal and non-terminal nodes, for summaries and exit codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    pub done: usize,
    pub failed: usize,
    pub skipped: usize,
    /// Pending and Running together — anything not yet resolved.
    pub outstanding: usize,
}

/// A run's state, derived purely from its event stream.
///
/// Nothing may live here that cannot be reconstructed from `events.jsonl`;
/// that invariant is what makes resume a replay.
#[derive(Debug, Clone, Default)]
pub struct RunState {
    pub nodes: BTreeMap<String, NodeState>,
    pub status: Option<RunStatus>,
}

impl RunState {
    pub fn new(ids: &[String]) -> Self {
        RunState {
            nodes: ids
                .iter()
                .map(|id| (id.clone(), NodeState::Pending))
                .collect(),
            status: None,
        }
    }

    pub fn state(&self, id: &str) -> NodeState {
        self.nodes.get(id).copied().unwrap_or(NodeState::Pending)
    }

    pub fn apply(&mut self, kind: &EventKind) {
        let transition = match kind {
            EventKind::NodeStarted { .. } => Some(NodeState::Running),
            EventKind::NodeFinished { .. } => Some(NodeState::Done),
            EventKind::NodeFailed { .. } => Some(NodeState::Failed),
            EventKind::NodeSkipped { .. } => Some(NodeState::Skipped),
            EventKind::RunStarted { .. } => None,
            EventKind::RunFinished { status } => {
                self.status = Some(*status);
                None
            }
        };

        if let (Some(node), Some(next)) = (kind.node(), transition) {
            self.nodes.insert(node.to_string(), next);
        }
    }

    pub fn replay<'a>(ids: &[String], events: impl IntoIterator<Item = &'a Event>) -> Self {
        events.into_iter().fold(RunState::new(ids), |mut st, e| {
            st.apply(&e.kind);
            st
        })
    }

    /// Nodes that were in flight when the process died must run again.
    pub fn reset_running(&mut self) -> Vec<String> {
        let interrupted: Vec<String> = self
            .nodes
            .iter()
            .filter(|(_, state)| **state == NodeState::Running)
            .map(|(id, _)| id.clone())
            .collect();

        self.nodes.extend(
            interrupted
                .iter()
                .map(|id| (id.clone(), NodeState::Pending)),
        );
        interrupted
    }

    /// A dependency satisfies a dependent when it is Done, or Failed on a task
    /// whose `on_failure` is `continue`. A Skipped dependency never satisfies.
    fn dependency_is_satisfied(&self, dep: &str, tasks: &TaskMap) -> bool {
        match self.state(dep) {
            NodeState::Done => true,
            NodeState::Failed => tasks
                .get(dep)
                .is_some_and(|t| t.on_failure == OnFailure::Continue),
            NodeState::Pending | NodeState::Running | NodeState::Skipped => false,
        }
    }

    pub fn ready(&self, dag: &Dag, tasks: &TaskMap) -> Vec<String> {
        dag.ids()
            .iter()
            .filter(|id| self.state(id) == NodeState::Pending)
            .filter(|id| {
                dag.needs(id)
                    .iter()
                    .all(|dep| self.dependency_is_satisfied(dep, tasks))
            })
            .cloned()
            .collect()
    }

    pub fn counts(&self) -> Counts {
        self.nodes
            .values()
            .fold(Counts::default(), |acc, state| match state {
                NodeState::Done => Counts {
                    done: acc.done + 1,
                    ..acc
                },
                NodeState::Failed => Counts {
                    failed: acc.failed + 1,
                    ..acc
                },
                NodeState::Skipped => Counts {
                    skipped: acc.skipped + 1,
                    ..acc
                },
                NodeState::Pending | NodeState::Running => Counts {
                    outstanding: acc.outstanding + 1,
                    ..acc
                },
            })
    }
}
