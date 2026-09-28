//! What one round runs, resolved by `run` in the process that runs it: the
//! repository's config applied to the job, its branch and its prompt.

use crate::config::{ConfigError, RepoConfig, parse_duration};
use crate::git::{self, PinnedRef};
use crate::job::JobId;
use crate::provider::{CommandSpec, render_command};
use std::path::Path;

/// The git credential a round in a container clones and pushes with.
/// Withheld from the agent's environment — see [`crate::exec`] — but an
/// agent running as the same user can still read it from `run`'s own
/// process environment; the spec lists that as an accepted risk.
pub const GIT_TOKEN_VAR: &str = "ASSEMBLY_GIT_TOKEN";

/// The forge credential `gh` opens a pull request with. Withheld from the
/// agent's environment like [`GIT_TOKEN_VAR`], and within its reach like it.
pub const FORGE_TOKEN_VAR: &str = "GH_TOKEN";

/// The HTTPS form of an SSH remote URL, which is what a token can
/// authenticate. Anything else — HTTPS already, a local path, `file://` —
/// comes back unchanged.
#[must_use]
pub fn https_equivalent(url: &str) -> String {
    let authority_and_path = match url.strip_prefix("ssh://") {
        Some(rest) => rest.split_once('/'),
        None => git::scp_like_parts(url),
    };

    match authority_and_path {
        Some((authority, path)) => {
            let host = authority.rsplit('@').next().unwrap_or(authority);
            let host = git::split_at_host_colon(host).map_or(host, |(host, _port)| host);
            // scp-like `host:/srv/r.git` names an absolute path, which a URL
            // carries with a single slash.
            format!("https://{host}/{}", path.trim_start_matches('/'))
        }
        None => url.to_string(),
    }
}

/// Whether a remote URL names a path on this machine — a plain path or
/// `file://` — rather than a host. Anything with a scheme other than
/// `file`, or git's scp-like `host:path`, reaches over the network.
#[must_use]
pub fn is_path_on_this_machine(url: &str) -> bool {
    match url.split_once("://") {
        Some((scheme, _)) => scheme == "file",
        None => git::scp_like_parts(url).is_none(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoundPayload {
    pub job_id: JobId,
    /// Where the round clones from and pushes to.
    pub remote_url: String,
    /// The name `BranchPushed` records that remote under.
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
    /// Whether the round runs `mise install` before the agent. Set by
    /// runners whose rounds arrive in an image with no toolchain of the
    /// repository's own.
    pub provision_toolchain: bool,
}

/// What `run` knows about a round before config has been applied to it.
#[derive(Debug, Clone)]
pub struct RoundRequest<'a> {
    pub job_id: JobId,
    pub prompt: &'a str,
    pub provider: &'a str,
    pub start: PinnedRef,
    pub remote_name: &'a str,
    pub remote_url: String,
}

impl RoundPayload {
    /// Apply the repository's config to a round.
    ///
    /// # Errors
    ///
    /// A [`ConfigError`] when the repository does not declare the provider,
    /// or declares a `max_duration` that is not a duration.
    pub fn for_round(
        config: &RepoConfig,
        request: RoundRequest<'_>,
    ) -> anyhow::Result<RoundPayload> {
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

        Ok(RoundPayload {
            job_id: request.job_id,
            remote_url: request.remote_url,
            remote_name: request.remote_name.to_string(),
            start: request.start,
            branch: request.job_id.branch_name(),
            command: render_command(provider, request.prompt),
            commit_message: commit_message(request.job_id, request.prompt),
            verify: config.verify.clone(),
            command_limit_secs,
            provision_toolchain: false,
        })
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
