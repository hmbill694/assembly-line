//! The review inbox: which nodes are waiting for a human, folded from the
//! event log.
//!
//! Review is a second axis, orthogonal to execution. A node is `Done` because
//! it ran; whether anyone has looked at it is tracked here. That separation is
//! what lets an unsupervised run defer its gates instead of stalling on them.
//!
//! Like [`crate::report`], this is a pure fold over events and renders
//! separately, so the whole thing is testable without running a graph.

use crate::event::{Event, EventKind};
use crate::report::DiffSummary;
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// Where a node stands with a human.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReviewState {
    /// The node declared no gate, so nobody is expected to look at it.
    #[default]
    NotGated,
    /// Merged on `verify` alone with its gate deferred. This is the inbox.
    Unreviewed,
    Approved,
    /// Sent back. The node's next round carries the feedback.
    RevisionRequested,
}

impl ReviewState {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::NotGated => "not gated",
            Self::Unreviewed => "unreviewed",
            Self::Approved => "approved",
            Self::RevisionRequested => "revision requested",
        }
    }
}

/// One node's standing with a reviewer, and what they need in order to judge
/// it: the branch to read and how much it changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewItem {
    pub node: String,
    pub state: ReviewState,
    /// The branch carrying the work — what a reviewer actually reads.
    pub branch: Option<String>,
    pub diff: Option<DiffSummary>,
    /// The most recent feedback given, when the node was sent back.
    pub feedback: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewInbox {
    pub run_id: u64,
    /// Every node the log mentions, in the order it was first seen.
    pub items: Vec<ReviewItem>,
}

/// One node's review facts, mid-fold.
#[derive(Debug, Clone, Default)]
struct ReviewProgress {
    state: ReviewState,
    branch: Option<String>,
    diff: Option<DiffSummary>,
    feedback: Option<String>,
}

impl ReviewProgress {
    fn after_event(self, kind: &EventKind) -> Self {
        match kind {
            EventKind::NodeCommitted {
                files,
                insertions,
                deletions,
                ..
            } => ReviewProgress {
                diff: Some(DiffSummary {
                    files: *files,
                    insertions: *insertions,
                    deletions: *deletions,
                }),
                ..self
            },
            EventKind::NodeBranchPublished { branch, .. } => ReviewProgress {
                branch: Some(branch.clone()),
                ..self
            },
            EventKind::NodeAwaitingReview { .. } => ReviewProgress {
                state: ReviewState::Unreviewed,
                ..self
            },
            EventKind::NodeApproved { .. } => ReviewProgress {
                state: ReviewState::Approved,
                ..self
            },
            EventKind::NodeRevisionRequested { feedback, .. } => ReviewProgress {
                state: ReviewState::RevisionRequested,
                feedback: Some(feedback.clone()),
                ..self
            },
            // A new round supersedes the last verdict: the node is back in
            // flight, and whatever it produces will be judged afresh.
            EventKind::NodeStarted { .. } => ReviewProgress {
                state: ReviewState::NotGated,
                diff: None,
                ..self
            },
            _ => self,
        }
    }
}

impl ReviewInbox {
    /// Fold an event stream into each node's review standing.
    #[must_use]
    pub fn from_events<'a>(run_id: u64, events: impl IntoIterator<Item = &'a Event>) -> Self {
        // Insertion order is the order nodes first appear, which is the order
        // they ran — more useful to a reviewer than alphabetical.
        let (progress, order) = events.into_iter().fold(
            (BTreeMap::<String, ReviewProgress>::new(), Vec::new()),
            |(mut progress, mut order), event| {
                if let Some(node) = event.kind.node() {
                    if !progress.contains_key(node) {
                        order.push(node.to_string());
                    }
                    let updated = progress
                        .get(node)
                        .cloned()
                        .unwrap_or_default()
                        .after_event(&event.kind);
                    progress.insert(node.to_string(), updated);
                }
                (progress, order)
            },
        );

        ReviewInbox {
            run_id,
            items: order
                .into_iter()
                .map(|node| {
                    let p = progress.get(&node).cloned().unwrap_or_default();
                    ReviewItem {
                        node,
                        state: p.state,
                        branch: p.branch,
                        diff: p.diff,
                        feedback: p.feedback,
                    }
                })
                .collect(),
        }
    }

    /// The inbox proper: nodes that merged with their gate deferred and have
    /// not been ruled on.
    #[must_use]
    pub fn awaiting_review(&self) -> Vec<&ReviewItem> {
        self.items
            .iter()
            .filter(|i| i.state == ReviewState::Unreviewed)
            .collect()
    }

    #[must_use]
    pub fn item(&self, node: &str) -> Option<&ReviewItem> {
        self.items.iter().find(|i| i.node == node)
    }

    /// What `assembly review` prints: the queue, then how to act on it.
    #[must_use]
    pub fn to_terminal_list(&self) -> String {
        let waiting = self.awaiting_review();
        if waiting.is_empty() {
            return format!("run {}: nothing awaiting review\n", self.run_id);
        }

        let node_column = waiting.iter().map(|i| i.node.len()).max().unwrap_or(0);
        let rows = waiting.iter().fold(String::new(), |mut out, item| {
            let _ = writeln!(
                out,
                "  ? {:<node_column$}  {:<16}  {}",
                item.node,
                item.diff.map(|d| d.to_string()).unwrap_or_default(),
                item.branch.as_deref().unwrap_or(""),
            );
            out
        });

        format!(
            "run {}: {} awaiting review\n\n{rows}\n\
               assembly review {} --approve <node>\n\
               assembly review {} --revise <node> \"what to change\"\n",
            self.run_id,
            waiting.len(),
            self.run_id,
            self.run_id,
        )
    }
}
