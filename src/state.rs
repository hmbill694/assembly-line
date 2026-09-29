/// A job's state, as `JobReport::from_events` derives it from the job's event
/// stream.
///
/// Nothing may live here that cannot be reconstructed from `events.jsonl`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum JobState {
    #[default]
    Pending,
    /// Asked for, and waiting for a slot to run in.
    Queued,
    Running,
    Passed,
    Failed,
}

impl JobState {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Passed => "passed",
            Self::Failed => "failed",
        }
    }
}
