use crate::event::{Event, EventKind, RunStatus};
use crate::state::NodeState;
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::Duration;

/// How much a node changed, as recorded when its work was committed.
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeReport {
    pub id: String,
    pub state: NodeState,
    /// Wall time of the node's most recent attempt.
    pub duration: Option<Duration>,
    /// What the node committed, for nodes that produced work. `None` for a
    /// shell node, or an agent that correctly decided nothing needed changing.
    pub diff: Option<DiffSummary>,
    /// Failure reason or skip cause, when there is one.
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunReport {
    pub id: u64,
    pub status: Option<RunStatus>,
    pub nodes: Vec<NodeReport>,
}

/// One node's facts, accumulated as the event stream is folded.
#[derive(Debug, Clone, Default)]
struct NodeProgress {
    state: Option<NodeState>,
    attempt_started_at: Option<DateTime<Utc>>,
    last_attempt_duration: Option<Duration>,
    committed_diff: Option<DiffSummary>,
    detail: Option<String>,
}

/// The whole run's progress, mid-fold.
#[derive(Debug, Default)]
struct RunProgress {
    nodes: BTreeMap<String, NodeProgress>,
    status: Option<RunStatus>,
}

impl RunProgress {
    /// Fold one event in. Written to be passed directly to `Iterator::fold`.
    fn after_event(mut self, event: &Event) -> Self {
        match &event.kind {
            EventKind::RunStarted { .. } => {}
            EventKind::RunFinished { status } => self.status = Some(*status),
            _ => {
                if let Some(node) = event.kind.node() {
                    let updated = self.progress_for(node).after_node_event(event);
                    self.nodes.insert(node.to_string(), updated);
                }
            }
        }
        self
    }

    fn progress_for(&self, node: &str) -> NodeProgress {
        self.nodes.get(node).cloned().unwrap_or_default()
    }
}

impl NodeProgress {
    fn after_node_event(self, event: &Event) -> Self {
        match &event.kind {
            // A new attempt restarts the clock and clears the previous reason
            // and diff, so a retried node reports its final attempt.
            EventKind::NodeStarted { .. } => NodeProgress {
                state: Some(NodeState::Running),
                attempt_started_at: Some(event.at),
                committed_diff: None,
                detail: None,
                ..self
            },
            EventKind::NodeCommitted {
                files,
                insertions,
                deletions,
                ..
            } => NodeProgress {
                committed_diff: Some(DiffSummary {
                    files: *files,
                    insertions: *insertions,
                    deletions: *deletions,
                }),
                ..self
            },
            EventKind::NodeFinished { .. } => NodeProgress {
                state: Some(NodeState::Done),
                last_attempt_duration: self.time_spent_until(event.at),
                ..self
            },
            EventKind::NodeFailed { reason, .. } => NodeProgress {
                state: Some(NodeState::Failed),
                last_attempt_duration: self.time_spent_until(event.at),
                detail: Some(reason.clone()),
                ..self
            },
            // A skipped node never started, so it has no duration.
            EventKind::NodeSkipped { because, .. } => NodeProgress {
                state: Some(NodeState::Skipped),
                detail: Some(because.clone()),
                ..self
            },
            // A merge moves the run branch, and publishing moves a ref —
            // neither is the node's own progress.
            EventKind::RunStarted { .. }
            | EventKind::RunBranchCreated { .. }
            | EventKind::NodeBranchPublished { .. }
            | EventKind::NodeMerged { .. }
            | EventKind::NodeMergeConflicted { .. }
            | EventKind::NodeAwaitingReview { .. }
            | EventKind::NodeApproved { .. }
            | EventKind::NodeRevisionRequested { .. }
            | EventKind::RunFinished { .. } => self,
        }
    }

    fn time_spent_until(&self, end: DateTime<Utc>) -> Option<Duration> {
        self.attempt_started_at
            .and_then(|start| (end - start).to_std().ok())
    }
}

impl RunReport {
    /// Fold an event stream into a per-node summary, in the graph's declared
    /// order. Nodes with no events yet appear as `Pending`.
    pub fn from_events<'a>(
        run_id: u64,
        ids: &[String],
        events: impl IntoIterator<Item = &'a Event>,
    ) -> Self {
        let progress = events
            .into_iter()
            .fold(RunProgress::default(), RunProgress::after_event);

        RunReport {
            id: run_id,
            status: progress.status,
            nodes: ids
                .iter()
                .map(|id| {
                    let node = progress.progress_for(id);
                    NodeReport {
                        id: id.clone(),
                        state: node.state.unwrap_or(NodeState::Pending),
                        duration: node.last_attempt_duration,
                        diff: node.committed_diff,
                        detail: node.detail,
                    }
                })
                .collect(),
        }
    }

    #[must_use]
    pub fn count_in_state(&self, want: NodeState) -> usize {
        self.nodes.iter().filter(|n| n.state == want).count()
    }

    /// A node tree plus a one-line summary, for `assembly status`.
    #[must_use]
    pub fn to_terminal_tree(&self) -> String {
        let id_column = self.nodes.iter().map(|n| n.id.len()).max().unwrap_or(0);
        // `None` for a run that committed nothing — a shell-only graph should
        // not pay for a column it never fills.
        let diff_column = self
            .nodes
            .iter()
            .filter_map(|n| n.diff)
            .map(|d| d.to_string().len())
            .max();

        let rows = self.nodes.iter().fold(String::new(), |mut out, node| {
            let _ = writeln!(
                out,
                "  {} {:<id_column$}  {:>7}{}  {}",
                state_glyph(node.state),
                node.id,
                format_duration(node.duration),
                format_diff_column(node.diff, diff_column),
                node.detail.as_deref().unwrap_or(""),
            );
            out
        });

        format!("{rows}\n{}\n", self.to_summary_line())
    }

    /// The single line printed at the end of a run.
    #[must_use]
    pub fn to_summary_line(&self) -> String {
        format!(
            "run {}: {} — {} done, {} failed, {} skipped",
            self.id,
            self.status.map_or("in progress", RunStatus::label),
            self.count_in_state(NodeState::Done),
            self.count_in_state(NodeState::Failed),
            self.count_in_state(NodeState::Skipped),
        )
    }
}

fn state_glyph(state: NodeState) -> char {
    match state {
        NodeState::Done => '✓',
        NodeState::Failed => '✗',
        NodeState::Skipped => '⊝',
        NodeState::Running => '⠙',
        NodeState::Pending => '·',
    }
}

/// The diff cell, padded to `width`, or nothing at all when the run has no
/// diff column.
fn format_diff_column(diff: Option<DiffSummary>, width: Option<usize>) -> String {
    width.map_or_else(String::new, |width| {
        let cell = diff.map(|d| d.to_string()).unwrap_or_default();
        format!("  {cell:<width$}")
    })
}

fn format_duration(duration: Option<Duration>) -> String {
    duration
        .map(|d| format!("{:.1}s", d.as_secs_f64()))
        .unwrap_or_default()
}
