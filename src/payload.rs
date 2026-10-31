//! What one round runs, resolved by `run` in the process that runs it: the
//! repository's config applied to the round, its job's branch and its
//! prompt.

use crate::config::JobSettings;
use crate::git::PinnedRef;
use crate::job::JobId;
use crate::provider::{CommandSpec, render_command};

/// The git credential a round in a container clones and pushes with.
/// Withheld from the agent's environment — see [`crate::exec`] — but an
/// agent running as the same user can still read it from `run`'s own
/// process environment; the spec lists that as an accepted risk.
pub const GIT_TOKEN_VAR: &str = "ASSEMBLY_GIT_TOKEN";

/// The forge credential `gh` opens a pull request with. Withheld from the
/// agent's environment like [`GIT_TOKEN_VAR`], and within its reach like it.
pub const FORGE_TOKEN_VAR: &str = "GH_TOKEN";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoundPayload {
    pub job_id: JobId,
    /// The commit the round starts from — the base for round 1, the job's
    /// branch tip for a revise.
    pub start: PinnedRef,
    pub command: CommandSpec,
    pub commit_message: String,
    pub verify: Option<String>,
    /// Cap on the agent, and again on `verify`, in whole seconds. See
    /// [`crate::config::RepoConfig::max_duration`].
    pub command_limit_secs: Option<u64>,
    /// Whether the round runs `mise install` before the agent. Set by
    /// runners whose rounds arrive in an image with no toolchain of the
    /// repository's own.
    pub provision_toolchain: bool,
}

/// What `run` knows about a round before its settings have been applied.
#[derive(Debug, Clone)]
pub struct RoundBeforeSettings<'a> {
    pub job_id: JobId,
    pub prompt: &'a str,
    pub start: PinnedRef,
    pub provision_toolchain: bool,
}

impl RoundPayload {
    /// Apply the job's settings to a round.
    #[must_use]
    pub fn for_round(settings: &JobSettings, round: RoundBeforeSettings<'_>) -> RoundPayload {
        RoundPayload {
            job_id: round.job_id,
            start: round.start,
            command: render_command(&settings.provider, round.prompt),
            commit_message: commit_message(round.job_id, round.prompt),
            verify: settings.verify.clone(),
            command_limit_secs: settings.max_duration.map(|limit| limit.as_secs()),
            provision_toolchain: round.provision_toolchain,
        }
    }
}

/// A commit message a human can scan in `git log` and an agent can learn
/// from: the job and the first non-blank line of what it was asked, then
/// the whole of what it was asked.
#[must_use]
pub fn commit_message(job_id: JobId, prompt: &str) -> String {
    match prompt.lines().find(|line| !line.trim().is_empty()) {
        Some(first) => format!("job {job_id}: {}\n\n{}", first.trim(), prompt.trim()),
        None => format!("job {job_id}: agent work"),
    }
}
