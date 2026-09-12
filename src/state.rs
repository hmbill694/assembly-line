use crate::event::{Event, EventKind};

/// A job's state, derived purely from its event stream.
///
/// Nothing may live here that cannot be reconstructed from `events.jsonl`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum JobState {
    #[default]
    Pending,
    Running,
    Succeeded,
    Failed,
}

impl JobState {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }

    pub fn apply(&mut self, kind: &EventKind) {
        *self = match kind {
            EventKind::JobStarted { .. } => JobState::Running,
            EventKind::JobFinished { .. } => JobState::Succeeded,
            EventKind::JobFailed { .. } => JobState::Failed,
            // Progress markers, not transitions. Listed one by one rather than
            // behind a catch-all, so a new event is a compile error here
            // instead of a silent omission.
            EventKind::JobCommitted { .. } | EventKind::JobBranchPublished { .. } => *self,
        };
    }

    #[must_use]
    pub fn replay<'a>(events: impl IntoIterator<Item = &'a Event>) -> Self {
        events.into_iter().fold(JobState::default(), |mut st, e| {
            st.apply(&e.kind);
            st
        })
    }
}
