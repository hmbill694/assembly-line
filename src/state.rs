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

    /// This state after `event`. Written to be passed directly to
    /// `Iterator::fold`.
    fn after_event(self, event: &Event) -> Self {
        match &event.kind {
            EventKind::JobStarted { .. } => JobState::Running,
            EventKind::JobFinished { .. } => JobState::Succeeded,
            EventKind::JobFailed { .. } => JobState::Failed,
            // Progress markers, not transitions. Listed one by one rather
            // than behind a catch-all, so a new event kind is a compile error
            // here instead of a silent omission.
            EventKind::JobCommitted { .. }
            | EventKind::JobBranchPublished { .. }
            | EventKind::JobVerifyFailed { .. } => self,
        }
    }

    #[must_use]
    pub fn replay<'a>(events: impl IntoIterator<Item = &'a Event>) -> Self {
        events
            .into_iter()
            .fold(JobState::default(), JobState::after_event)
    }
}
