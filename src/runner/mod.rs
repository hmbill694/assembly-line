//! Where a job runs, and how its stream comes back.

pub mod child;
pub mod docker;
pub mod kubernetes;
pub mod local;

use crate::payload::{GIT_TOKEN_VAR, JobPayload};
use std::collections::{BTreeMap, BTreeSet};

/// Where the image a container runner launches is published.
pub const PUBLISHED_IMAGE_REPOSITORY: &str = "ghcr.io/hmbill694/assembly-line";

/// A name unique to this round, for the container or Job that runs it, so
/// two repositories' job 1 never collide.
fn job_resource_name(payload: &JobPayload) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("al-{}-{}-{nanos:x}", payload.job_id, payload.round)
}

/// The image published for this binary's version. For a release, its
/// `job-exec` is built from the same tag, so the collector and the job agree
/// about the frame format; a build between releases carries the last version
/// number and may have moved past that image — `--image` names a closer one.
#[must_use]
pub fn published_image() -> String {
    format!("{PUBLISHED_IMAGE_REPOSITORY}:{}", env!("CARGO_PKG_VERSION"))
}

/// One way of running `job-exec` somewhere.
pub trait Runner {
    type Running: RunningJob + Send;

    /// Whether jobs run somewhere sharing nothing with the host. Decides
    /// whether the payload provisions a toolchain, whether the remote URL
    /// must suit a token, whether `copy` can work, and whether the git
    /// credential has to be sent along.
    const RUNS_IN_A_CONTAINER: bool;

    /// Every reason this runner cannot launch a job right now, checked
    /// before a job directory is allocated.
    fn reasons_it_cannot_run(&self) -> impl Future<Output = Vec<RunnerProblem>> + Send;

    fn launch(
        &self,
        payload: &JobPayload,
        secrets: &JobSecrets,
    ) -> impl Future<Output = anyhow::Result<Self::Running>> + Send;
}

/// A launched job: its stream, a way to stop it, and why it stopped.
pub trait RunningJob {
    /// The next line of the job's merged output, or `None` at its end. A
    /// runner whose stream can drop reconnects inside this.
    fn next_line(&mut self) -> impl Future<Output = Option<String>> + Send;
    /// Start stopping the job, and return without waiting for it to stop:
    /// the caller keeps reading [`RunningJob::next_line`] until the stream
    /// ends, and a job winding down may print more than any pipe holds.
    fn cancel(&mut self) -> impl Future<Output = ()> + Send;
    fn termination(self) -> impl Future<Output = Termination> + Send;
}

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

/// Something that stops a runner from launching a job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunnerProblem {
    Unreachable {
        runner: &'static str,
        detail: String,
    },
    NotPermitted {
        verb: String,
        resource: String,
        namespace: String,
    },
    CopyNeedsLocalRunner,
    MissingEnvironment(String),
}

impl std::fmt::Display for RunnerProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable { runner, detail } => write!(
                f,
                "`{runner}` cannot be reached — install it or check its context: {detail}"
            ),
            Self::NotPermitted {
                verb,
                resource,
                namespace,
            } => write!(
                f,
                "cannot {verb} {resource} in namespace '{namespace}' — grant the permission, \
                 or pass a --namespace where you have it"
            ),
            Self::CopyNeedsLocalRunner => write!(
                f,
                "this repository declares `copy`, which reads files from your checkout — a \
                 container has no access to it; use --runner local"
            ),
            Self::MissingEnvironment(name) => write!(
                f,
                "${name} is not set — export it, since the job's container receives it from \
                 your environment"
            ),
        }
    }
}

/// Environment a container job receives, by name. Read from the host once,
/// here, and only for names the host chose.
#[derive(Debug, Clone, Default)]
pub struct JobSecrets {
    vars: BTreeMap<String, String>,
}

impl JobSecrets {
    /// The git credential, always, plus each name in `pass_env`, looked up
    /// with `lookup`. Every name that has no value is a problem.
    pub fn from_lookup(
        pass_env: &[String],
        lookup: impl Fn(&str) -> Option<String>,
    ) -> (JobSecrets, Vec<RunnerProblem>) {
        let names: Vec<&str> = std::iter::once(GIT_TOKEN_VAR)
            .chain(pass_env.iter().map(String::as_str))
            .collect();
        let (found, missing): (Vec<_>, Vec<_>) = names
            .into_iter()
            .map(|name| (name, lookup(name)))
            .partition(|(_, value)| value.is_some());

        (
            JobSecrets {
                vars: found
                    .into_iter()
                    .filter_map(|(name, value)| Some((name.to_string(), value?)))
                    .collect(),
            },
            missing
                .into_iter()
                .map(|(name, _)| RunnerProblem::MissingEnvironment(name.to_string()))
                .collect(),
        )
    }

    #[must_use]
    pub fn from_host_environment(pass_env: &[String]) -> (JobSecrets, Vec<RunnerProblem>) {
        Self::from_lookup(pass_env, |name| std::env::var(name).ok())
    }

    #[must_use]
    pub fn names(&self) -> BTreeSet<String> {
        self.vars.keys().cloned().collect()
    }

    #[must_use]
    pub fn vars(&self) -> &BTreeMap<String, String> {
        &self.vars
    }
}

/// What a repository asks for that no container can give it.
#[must_use]
pub fn reasons_a_container_cannot_run(copy: &[String]) -> Vec<RunnerProblem> {
    (!copy.is_empty())
        .then_some(RunnerProblem::CopyNeedsLocalRunner)
        .into_iter()
        .collect()
}
