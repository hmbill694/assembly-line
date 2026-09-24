//! Everything a round needs to run, resolved by the host before it starts.
//!
//! The payload is the whole plan. `job-exec` runs the round, wherever its
//! runner puts it, and executes exactly what it says, reading no
//! configuration of its own, so nothing inside the boundary can influence
//! the settings that govern it.

use crate::config::{ConfigError, RepoConfig, parse_duration};
use crate::git::{self, PinnedRef};
use crate::provider::{CommandSpec, render_command};
use crate::workspace::job_branch_name;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The environment variable a payload travels in: the one channel a child
/// process, `docker run` and a pod spec all share.
pub const PAYLOAD_VAR: &str = "ASSEMBLY_JOB";

/// The git credential a container job clones and pushes with. Withheld from
/// the agent's environment — see [`crate::exec`] — but an agent running as
/// the same user can still read it from `job-exec`'s own process
/// environment; the spec lists that as an accepted risk.
pub const GIT_TOKEN_VAR: &str = "ASSEMBLY_GIT_TOKEN";

/// `s` split at the colon that ends a host: the first one outside a
/// bracketed IPv6 literal, whose own colons end nothing.
fn split_at_host_colon(s: &str) -> Option<(&str, &str)> {
    let searched_from = match (s.find('['), s.find(':')) {
        (Some(open), Some(colon)) if open < colon => {
            s[open..].find(']').map_or(open, |close| open + close)
        }
        _ => 0,
    };
    let colon = searched_from + s[searched_from..].find(':')?;
    Some((&s[..colon], &s[colon + 1..]))
}

/// The HTTPS form of an SSH remote URL, which is what a token can
/// authenticate. Anything else — HTTPS already, a local path, `file://` —
/// comes back unchanged.
#[must_use]
pub fn https_equivalent(url: &str) -> String {
    let authority_and_path = match url.strip_prefix("ssh://") {
        Some(rest) => rest.split_once('/'),
        // scp-like `host:path`, which git recognises by a colon before any
        // slash. A local path has no such colon.
        None if !url.contains("://") => {
            split_at_host_colon(url).filter(|(authority, _)| !authority.contains('/'))
        }
        None => None,
    };

    match authority_and_path {
        Some((authority, path)) => {
            let host = authority.rsplit('@').next().unwrap_or(authority);
            let host = split_at_host_colon(host).map_or(host, |(host, _port)| host);
            // scp-like `host:/srv/r.git` names an absolute path, which a URL
            // carries with a single slash.
            format!("https://{host}/{}", path.trim_start_matches('/'))
        }
        None => url.to_string(),
    }
}

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
