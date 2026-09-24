//! Where a job runs, and how its stream comes back.

pub mod child;
pub mod local;

/// Why a job's process stopped, as its runner can tell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Termination {
    Exited(i32),
    /// Stopped by something other than its own exit — out of memory,
    /// evicted, deleted. `reason` is the runner's own word for it.
    Killed {
        reason: String,
    },
}

impl std::fmt::Display for Termination {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exited(code) => write!(f, "exit {code}"),
            Self::Killed { reason } => write!(f, "{reason}"),
        }
    }
}
