//! Everything a round needs to run, resolved by the host before it starts.
//!
//! The payload is the whole plan. Whoever runs the round — this process
//! today, `job-exec` in a container from Task 6 on — executes exactly what
//! it says and reads no configuration of its own, so nothing inside the
//! boundary can influence the settings that govern it.

use crate::config::{ConfigError, RepoConfig, parse_duration};
use crate::git::{self, PinnedRef};
use crate::provider::{CommandSpec, render_command};
use crate::workspace::job_branch_name;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The environment variable a payload travels in: the one channel a child
/// process, `docker run` and a pod spec all share.
pub const PAYLOAD_VAR: &str = "ASSEMBLY_JOB";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobPayload {
    pub job_id: u64,
    pub round: u32,
    /// Where the round clones from and pushes to.
    pub remote_url: String,
    /// The host's name for that remote, which `JobBranchPublished` records.
    pub remote_name: String,
    /// The commit the round starts from — the base for round 1, the job's
    /// branch tip for a revise.
    pub start: PinnedRef,
    pub branch: String,
    pub command: CommandSpec,
    pub commit_message: String,
    pub verify: Option<String>,
    /// Cap on the agent, and again on `verify`, in whole seconds. See
    /// [`RepoConfig::max_duration`].
    pub command_limit_secs: Option<u64>,
    pub copy: Vec<String>,
    /// Where `copy` paths resolve from — the host's checkout, which only a
    /// runner sharing the host's filesystem can read.
    pub seed_from: PathBuf,
    /// Whether the round runs `mise install` before the agent. Set by
    /// runners whose jobs arrive in an image with no toolchain of the
    /// repository's own.
    pub provision_toolchain: bool,
}

/// What the host knows about a round before config has been applied to it.
#[derive(Debug, Clone)]
pub struct RoundRequest<'a> {
    pub job_id: u64,
    pub round: u32,
    pub prompt: &'a str,
    pub provider: &'a str,
    pub start: PinnedRef,
    pub remote_name: &'a str,
    pub remote_url: String,
    pub seed_from: &'a Path,
}

impl JobPayload {
    /// Apply the repository's config to a round.
    ///
    /// # Errors
    ///
    /// A [`ConfigError`] when the repository does not declare the provider,
    /// or declares a `max_duration` that is not a duration.
    pub fn for_round(config: &RepoConfig, request: RoundRequest<'_>) -> anyhow::Result<JobPayload> {
        let provider = config
            .providers
            .get(request.provider)
            .ok_or_else(|| ConfigError::UnknownProvider(request.provider.to_string()))?;
        let command_limit_secs = config
            .max_duration
            .as_deref()
            .map(parse_duration)
            .transpose()?
            .map(|limit| limit.as_secs());

        Ok(JobPayload {
            job_id: request.job_id,
            round: request.round,
            remote_url: request.remote_url,
            remote_name: request.remote_name.to_string(),
            start: request.start,
            branch: job_branch_name(request.job_id),
            command: render_command(provider, request.prompt),
            commit_message: commit_message(request.job_id, request.prompt),
            verify: config.verify.clone(),
            command_limit_secs,
            copy: config.copy.clone(),
            seed_from: request.seed_from.to_path_buf(),
            provision_toolchain: false,
        })
    }
}

/// A commit subject a human can scan in `git log`: the job, then the first
/// non-blank line of what it was asked to do.
fn commit_message(job_id: u64, prompt: &str) -> String {
    match prompt.lines().find(|line| !line.trim().is_empty()) {
        Some(first) => format!("job {job_id}: {}", first.trim()),
        None => format!("job {job_id}: agent work"),
    }
}

/// The prompt a revise round carries: what was originally asked, then what to
/// change about the answer.
///
/// That is enough because a revise is a *new round*, not a resumption.
/// Nothing is kept from the last round except the branch: the agent's prior
/// work arrives as files on disk, already committed in the tree it is dropped
/// into. No session replay, no conversation history — which is what makes
/// revising behave identically across every provider.
#[must_use]
pub fn revised_prompt(original: &str, feedback: &str) -> String {
    format!(
        "{original}\n\n---\n\nYour previous attempt is already committed in this \
         working tree. Revise it based on this feedback:\n\n{feedback}\n"
    )
}

/// Where the round clones from. A repository without the remote cannot run a
/// job at all: the job clones from it and pushes its branch back to it.
///
/// # Errors
///
/// An error saying to add the remote when the repository has none, or git's
/// own when the remote cannot be listed.
pub async fn remote_to_clone(repo: &Path, remote: &str) -> anyhow::Result<String> {
    git::remote_url(repo, remote).await?.ok_or_else(|| {
        anyhow::anyhow!(
            "the repository has no '{remote}' remote — a job clones from it and pushes its \
             branch back to it, so add one"
        )
    })
}
