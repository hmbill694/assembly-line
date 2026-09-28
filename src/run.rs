//! `assembly run`: one whole job, in this process — what every runner
//! launches, and what a person types to reproduce a job.
//!
//! It keeps no state. What it did is its output — frames for a collector, or
//! readable lines for a person — and the branch and pull request it leaves.
//! Preparing claims nothing until every check that can refuse the job has
//! passed, so a refusal costs no id.

use crate::claim::claim_job;
use crate::config::{self, ConfigError, RepoConfig};
use crate::delivery::{self, Delivered, PullRequestText};
use crate::event::EventKind;
use crate::frame::FrameWriter;
use crate::git::{self, PinnedRef};
use crate::job::JobId;
use crate::payload::{RoundPayload, RoundRequest, remote_to_clone};
use crate::report::JobReport;
use crate::round::{
    PROVISIONING_LIMIT, Verdict, credential_helper_for_this_environment, discard_reporting_failure,
    run_round_in, within_provisioning_limit,
};
use crate::workspace::{self, DEFAULT_REMOTE, RoundWorkspace, ScratchClone};
use anyhow::anyhow;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tokio_util::sync::CancellationToken;

/// What `assembly run` was asked for, as given on the command line.
#[derive(Debug, Clone, Default)]
pub struct RunRequest {
    /// A checkout, or a remote URL. `None` is the enclosing checkout.
    pub repo: Option<String>,
    /// `REF`, or `REF@SHA` to pin it.
    pub base_ref: Option<String>,
    pub job: Option<u64>,
    pub prompt: Option<String>,
    pub prompt_file: Option<PathBuf>,
    pub provider: Option<String>,
    pub provision_toolchain: bool,
}

/// Why a job will not run, decided before anything was claimed for it.
#[derive(Debug)]
pub enum RunRefused {
    ConfigNotRunnable(Vec<ConfigError>),
    NoJobBranch(JobId),
    Unpreparable(anyhow::Error),
}

impl RunRefused {
    /// Every reason, one per line, to report ahead of the refusal itself.
    #[must_use]
    pub fn itemized_reasons(&self) -> Vec<String> {
        match self {
            Self::ConfigNotRunnable(errors) => errors.iter().map(ToString::to_string).collect(),
            Self::NoJobBranch(_) | Self::Unpreparable(_) => Vec::new(),
        }
    }
}

impl std::fmt::Display for RunRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConfigNotRunnable(_) => write!(f, "{} is not runnable", config::REPO_CONFIG_PATH),
            Self::NoJobBranch(id) => write!(
                f,
                "job {id} has no branch on '{DEFAULT_REMOTE}' — it was deleted, or the job never \
                 pushed one, so there is nothing to continue; run without --job to start a new job"
            ),
            Self::Unpreparable(e) => write!(f, "{e}"),
        }
    }
}

/// The pull request targets a different branch from the one the job was cut
/// from, so its diff carries more than the job's work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseDiffers {
    pub base: String,
    pub base_ref: String,
}

impl std::fmt::Display for BaseDiffers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "this pull request will target '{}', but the job was cut from '{}' — review the \
             diff before merging, since it carries everything separating the two, not just this \
             job's work",
            self.base, self.base_ref
        )
    }
}

/// A job that has passed every check: cloned, its config read and valid, its
/// id claimed or its branch found.
#[derive(Debug)]
pub struct ReadyJob {
    clone: ScratchClone,
    payload: RoundPayload,
    config: RepoConfig,
    base: PinnedRef,
    prompt: String,
    claimed: bool,
    provisioning_deadline: Instant,
}

/// What a finished job left.
#[derive(Debug)]
pub struct JobConclusion {
    pub job: JobId,
    pub verdict: Verdict,
    pub report: JobReport,
    pub base_differs: Option<BaseDiffers>,
}

impl JobConclusion {
    /// What the job left, as printed once it is over.
    #[must_use]
    pub fn to_lines(&self) -> Vec<String> {
        self.base_differs
            .iter()
            .map(|differs| format!("note: {differs}"))
            .chain(self.report.to_status_lines())
            .collect()
    }
}

/// Everything `run` checks, and the claim, before the round.
///
/// # Errors
///
/// A [`RunRefused`] for every reason the job cannot run; nothing is claimed
/// on the remote for a refused job.
pub async fn prepare_run(
    request: RunRequest,
    scratch_root: &Path,
    cancel: &CancellationToken,
) -> Result<ReadyJob, RunRefused> {
    let prompt =
        prompt_text(request.prompt, request.prompt_file).map_err(RunRefused::Unpreparable)?;
    let (remote_url, base_ref) = remote_and_base_ref(request.repo, request.base_ref)
        .await
        .map_err(RunRefused::Unpreparable)?;
    let (ref_name, pinned_sha) = split_pin(&base_ref);

    let provisioning_deadline = Instant::now() + PROVISIONING_LIMIT;
    let clone = within_provisioning_limit(
        workspace::clone_scratch(
            &remote_url,
            scratch_root,
            credential_helper_for_this_environment(),
        ),
        cancel,
    )
    .await
    .map_err(RunRefused::Unpreparable)?;

    let base = workspace::pin_in_clone(&clone, &ref_name, pinned_sha.as_deref())
        .await
        .map_err(RunRefused::Unpreparable)?;
    let config = RepoConfig::from_ref(clone.path(), &base.sha)
        .await
        .map_err(RunRefused::Unpreparable)?;
    let provider = request
        .provider
        .or_else(|| config.provider.clone())
        .unwrap_or_default();
    let problems = config.reasons_it_cannot_run(&provider);
    if !problems.is_empty() {
        return Err(RunRefused::ConfigNotRunnable(problems));
    }

    // Pinning and reading the config change nothing on the remote; claiming
    // does, so a job cancelled by now is not claimed.
    if cancel.is_cancelled() {
        return Err(RunRefused::Unpreparable(anyhow!("cancelled")));
    }
    let (job_id, start, claimed) = match request.job.map(JobId::from) {
        Some(id) => (id, job_branch_tip(&clone, id).await?, false),
        None => (
            claim_job(clone.path(), DEFAULT_REMOTE, &base.sha)
                .await
                .map_err(RunRefused::Unpreparable)?,
            base.clone(),
            true,
        ),
    };

    let payload = RoundPayload::for_round(
        &config,
        RoundRequest {
            job_id,
            // `run` numbers no rounds; the field goes with `job-exec`.
            round: 0,
            prompt: &prompt,
            provider: &provider,
            start,
            remote_name: DEFAULT_REMOTE,
            remote_url,
        },
    )
    .map_err(RunRefused::Unpreparable)?;

    Ok(ReadyJob {
        clone,
        payload: RoundPayload {
            provision_toolchain: request.provision_toolchain,
            ..payload
        },
        config,
        base,
        prompt,
        claimed,
        provisioning_deadline,
    })
}

/// Where continuing job `id` starts: its branch's tip on the remote.
async fn job_branch_tip(clone: &ScratchClone, id: JobId) -> Result<PinnedRef, RunRefused> {
    workspace::job_branch_in_clone(clone, &id.branch_name())
        .await
        .map_err(RunRefused::Unpreparable)?
        .ok_or(RunRefused::NoJobBranch(id))
}

impl ReadyJob {
    /// Which job is about to run, and from where.
    #[must_use]
    pub fn to_announcement_line(&self) -> String {
        match self.claimed {
            true => format!(
                "job {}: claimed {} at {} ({})",
                self.payload.job_id,
                self.payload.branch,
                self.base.name,
                &self.base.sha[..12.min(self.base.sha.len())]
            ),
            false => format!(
                "job {}: continuing {}",
                self.payload.job_id, self.payload.branch
            ),
        }
    }

    /// Run the round, deliver its branch if it passed, and conclude.
    ///
    /// # Errors
    ///
    /// Only when the round's frames cannot be written.
    pub async fn run<W: Write + Send + 'static>(
        self,
        frames: &FrameWriter<W>,
        cancel: CancellationToken,
    ) -> anyhow::Result<JobConclusion> {
        let ReadyJob {
            clone,
            payload,
            config,
            base,
            prompt,
            provisioning_deadline,
            ..
        } = self;
        let ws = match workspace::start_round(clone, &payload.start, &payload.branch).await {
            Ok(ws) => ws,
            Err(e) => {
                frames.append_event(EventKind::RoundFailed {
                    reason: e.to_string(),
                })?;
                return Ok(conclusion_of(&payload, frames, Verdict::Failed, None));
            }
        };
        let verdict = run_round_in(&payload, &ws, frames, provisioning_deadline, cancel).await?;

        let base_differs = match verdict {
            Verdict::Passed if branch_carries_work(&ws, &payload, &base, frames).await => {
                deliver(&ws, &config, &base, &payload, &prompt, frames).await?
            }
            Verdict::Passed | Verdict::Failed => None,
        };
        discard_reporting_failure(ws, frames)?;
        Ok(conclusion_of(&payload, frames, verdict, base_differs))
    }
}

/// Whether the job's branch, as the round left it on the remote, holds
/// commits its base does not — this round's, or an earlier round's when this
/// one changed nothing. A branch that could not be compared holds nothing
/// worth delivering.
async fn branch_carries_work<W: Write>(
    ws: &RoundWorkspace,
    payload: &RoundPayload,
    base: &PinnedRef,
    frames: &FrameWriter<W>,
) -> bool {
    let tip = frames
        .events_so_far()
        .into_iter()
        .rev()
        .find_map(|e| match e.kind {
            EventKind::RoundCommitted { sha, .. } => Some(sha),
            _ => None,
        })
        .unwrap_or_else(|| payload.start.sha.clone());
    git::commit_is_ahead_of(ws.path(), &tip, &base.sha)
        .await
        .unwrap_or(false)
}

/// Open the pull request from inside the clone, and record what came of it:
/// an event for a pull request that exists, a line of output otherwise.
async fn deliver<W: Write + Send + 'static>(
    ws: &RoundWorkspace,
    config: &RepoConfig,
    base: &PinnedRef,
    payload: &RoundPayload,
    prompt: &str,
    frames: &FrameWriter<W>,
) -> anyhow::Result<Option<BaseDiffers>> {
    let target = config.base.as_deref().unwrap_or(&base.name);
    let base_differs = (target != base.name).then(|| BaseDiffers {
        base: target.to_string(),
        base_ref: base.name.clone(),
    });
    let delivered = delivery::deliver(
        ws.path(),
        &config.delivery,
        &payload.branch,
        target,
        PullRequestText {
            title: payload.commit_message.lines().next().unwrap_or_default(),
            body: prompt,
        },
    )
    .await;
    match delivered {
        Delivered::Opened { url } | Delivered::AlreadyOpen { url } => {
            frames.append_event(EventKind::PullRequestOpened { url })?;
        }
        other => frames.append_output(&other.to_string())?,
    }
    Ok(base_differs)
}

fn conclusion_of<W: Write>(
    payload: &RoundPayload,
    frames: &FrameWriter<W>,
    verdict: Verdict,
    base_differs: Option<BaseDiffers>,
) -> JobConclusion {
    JobConclusion {
        job: payload.job_id,
        verdict,
        report: JobReport::from_events(payload.job_id.into(), &frames.events_so_far()),
        base_differs,
    }
}

/// The prompt, from the command line or the file it names.
///
/// # Errors
///
/// When the file cannot be read.
pub fn prompt_text(prompt: Option<String>, prompt_file: Option<PathBuf>) -> anyhow::Result<String> {
    match (prompt, prompt_file) {
        (Some(text), _) => Ok(text),
        (None, Some(path)) => {
            std::fs::read_to_string(&path).map_err(|e| anyhow!("reading {}: {e}", path.display()))
        }
        // clap refuses this combination before we are reached.
        (None, None) => Err(anyhow!("a job needs --prompt or --prompt-file")),
    }
}

/// Where the job clones from, and what it starts from: `base_ref` when one
/// is named, else a checkout's checked-out branch. A URL has none.
async fn remote_and_base_ref(
    repo: Option<String>,
    base_ref: Option<String>,
) -> anyhow::Result<(String, String)> {
    let (remote_url, checkout) = match repo {
        Some(url) if !Path::new(&url).join(".git").exists() => (url, None),
        named => {
            let checkout = crate::locate::checkout_named_or_enclosing(named.map(PathBuf::from))?;
            (
                remote_to_clone(&checkout, DEFAULT_REMOTE).await?,
                Some(checkout),
            )
        }
    };
    let base_ref = match (base_ref, checkout) {
        (Some(named), _) => named,
        (None, Some(checkout)) => git::current_branch(&checkout)
            .await?
            .ok_or_else(|| anyhow!("HEAD is detached — name the ref to start from with --ref"))?,
        (None, None) => {
            return Err(anyhow!(
                "a remote URL has no checked-out branch — name the ref to start from with --ref"
            ));
        }
    };
    Ok((remote_url, base_ref))
}

/// `main@<sha>` as the ref and the commit it is pinned to; anything whose
/// part after the last `@` is not a full hex object name is a ref alone.
fn split_pin(named: &str) -> (String, Option<String>) {
    match named.rsplit_once('@') {
        Some((name, sha))
            if !name.is_empty()
                && matches!(sha.len(), 40 | 64)
                && sha.chars().all(|c| c.is_ascii_hexdigit()) =>
        {
            (name.to_string(), Some(sha.to_string()))
        }
        _ => (named.to_string(), None),
    }
}
