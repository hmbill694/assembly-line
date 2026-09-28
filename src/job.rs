//! A job's identity: its id, and the branch its rounds publish to.

use serde::{Deserialize, Serialize};

/// A job's id, as [`crate::paths::next_job_id`] allocates it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JobId(u64);

const BRANCH_PREFIX: &str = "al/job-";

impl JobId {
    /// Matches every name [`Self::branch_name`] makes, for asking a remote
    /// which jobs it already carries.
    pub const BRANCH_PATTERN: &str = "al/job-*";

    /// The branch this job's rounds publish to. Git refs are paths, so this
    /// must never nest under another ref assembly-line creates.
    #[must_use]
    pub fn branch_name(self) -> String {
        format!("{BRANCH_PREFIX}{}", self.0)
    }

    /// The job a branch name was made for by [`Self::branch_name`], or `None`
    /// for any branch that is not a job's — including near misses like
    /// `al/job-007`, which that function never makes.
    #[must_use]
    pub fn from_branch_name(branch: &str) -> Option<JobId> {
        let id = JobId(branch.strip_prefix(BRANCH_PREFIX)?.parse().ok()?);
        (id.branch_name() == branch).then_some(id)
    }
}

impl From<u64> for JobId {
    fn from(id: u64) -> Self {
        JobId(id)
    }
}

impl From<JobId> for u64 {
    fn from(id: JobId) -> Self {
        id.0
    }
}

impl std::fmt::Display for JobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
