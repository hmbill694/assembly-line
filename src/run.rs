//! `assembly run`: one whole job, in this process — what every runner
//! launches, and what a person types to reproduce a job.
//!
//! It keeps no state. What it did is its output — frames for a collector, or
//! readable lines for a person — and the branch and pull request it leaves.
//! Preparing claims nothing until every check that can refuse the job has
//! passed, so a refusal costs no id.

use crate::admission::{self, AdmissionRefused};
use crate::claim::claim_job;
use crate::config::JobSettings;
use crate::delivery::{self, BaseDiffers, Delivered, DeliveryPlan, PullRequestText};
use crate::event::EventKind;
use crate::frame::FrameWriter;
use crate::git::{self, PinnedRef};
use crate::job::JobId;
use crate::payload::{RoundPayload, RoundRequest, remote_to_clone};
use crate::report::JobReport;
use crate::round::{FinishedRound, Verdict, run_round};
use crate::workspace::{self, DEFAULT_REMOTE, ScratchClone};
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
    NotAdmitted(AdmissionRefused),
    NoJobBranch(JobId),
    Unpreparable(anyhow::Error),
}

impl RunRefused {
    /// Every reason, one per line, to report ahead of the refusal itself.
    #[must_use]
    pub fn itemized_reasons(&self) -> Vec<String> {
        match self {
            Self::NotAdmitted(refused) => refused.itemized_reasons(),
            Self::NoJobBranch(_) | Self::Unpreparable(_) => Vec::new(),
        }
    }

    /// Why the round failed, as its verdict records it: the fault, without
    /// advice that only makes sense to whoever typed `run`.
    #[must_use]
    pub fn to_failure_reason(&self) -> String {
        match self {
            Self::NotAdmitted(refused) => refused.to_failure_reason(),
            Self::NoJobBranch(id) => no_job_branch(*id),
            Self::Unpreparable(e) => e.to_string(),
        }
    }
}

impl std::fmt::Display for RunRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAdmitted(refused) => write!(f, "{refused}"),
            Self::NoJobBranch(id) => write!(
                f,
                "{}, so there is nothing to continue; run without --job to start a new job",
                no_job_branch(*id)
            ),
            Self::Unpreparable(e) => write!(f, "{e}"),
        }
    }
}

fn no_job_branch(id: JobId) -> String {
    format!(
        "job {id} has no branch on '{DEFAULT_REMOTE}' — it was deleted, or the job never pushed \
         one"
    )
}

/// A job that has passed every check: cloned, its config read and valid, its
/// id claimed or its branch found.
#[derive(Debug)]
pub struct ReadyJob {
    clone: ScratchClone,
    payload: RoundPayload,
    settings: JobSettings,
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
            .chain(self.report.to_conclusion_lines())
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

    let (clone, provisioning_deadline) =
        workspace::clone_within_provisioning_limit(&remote_url, scratch_root, cancel)
            .await
            .map_err(RunRefused::Unpreparable)?;

    let base = workspace::pin_in_clone(&clone, &ref_name, pinned_sha.as_deref())
        .await
        .map_err(RunRefused::Unpreparable)?;
    let admitted = admission::admit(clone.path(), base, request.provider.as_deref())
        .await
        .map_err(RunRefused::NotAdmitted)?;

    // Pinning and admitting change nothing on the remote; claiming does, so
    // a job cancelled by now is not claimed.
    if cancel.is_cancelled() {
        return Err(RunRefused::Unpreparable(anyhow!("cancelled")));
    }
    let (job_id, start, claimed) = match request.job.map(JobId::from) {
        Some(id) => (id, job_branch_tip(&clone, id).await?, false),
        None => (
            claim_job(clone.path(), DEFAULT_REMOTE, &admitted)
                .await
                .map_err(RunRefused::Unpreparable)?,
            admitted.base().clone(),
            true,
        ),
    };

    let (base, settings) = admitted.into_base_and_settings();
    let payload = RoundPayload::for_round(
        &settings,
        RoundRequest {
            job_id,
            prompt: &prompt,
            start,
            remote_name: DEFAULT_REMOTE,
            remote_url,
        },
    );

    Ok(ReadyJob {
        clone,
        payload: RoundPayload {
            provision_toolchain: request.provision_toolchain,
            ..payload
        },
        settings,
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

    /// Run the round, deliver its branch if it passed, and only then report
    /// its verdict: a collector that sees the verdict has seen everything
    /// the round will report, and one that does not keeps reading.
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
            settings,
            base,
            prompt,
            provisioning_deadline,
            ..
        } = self;
        let round = run_round(
            clone,
            &payload,
            frames,
            provisioning_deadline,
            cancel.clone(),
        )
        .await?;
        let plan = delivery::delivery_plan(
            round.verdict(),
            round.branch_tip(),
            &base,
            settings.pull_request_target.as_deref(),
        );
        let base_differs = match (plan, round.workspace_path()) {
            (
                DeliveryPlan::OpenPullRequest {
                    target,
                    base_differs,
                },
                Some(checkout),
            ) => {
                deliver(
                    checkout, &settings, &target, &payload, &prompt, frames, &cancel,
                )
                .await?;
                base_differs
            }
            (DeliveryPlan::NothingToDeliver | DeliveryPlan::OpenPullRequest { .. }, _) => None,
        };
        let verdict = closing_after_delivery(round, &cancel).close(frames)?;
        Ok(conclusion_of(&payload, frames, verdict, base_differs))
    }
}

/// Open the pull request from inside `checkout`, and record what came of
/// it: an event for a pull request that exists, a line of output otherwise.
async fn deliver<W: Write + Send + 'static>(
    checkout: &Path,
    settings: &JobSettings,
    target: &str,
    payload: &RoundPayload,
    prompt: &str,
    frames: &FrameWriter<W>,
    cancel: &CancellationToken,
) -> anyhow::Result<()> {
    let delivered = delivery::deliver(
        checkout,
        &settings.delivery,
        &payload.branch,
        target,
        PullRequestText {
            title: payload.commit_message.lines().next().unwrap_or_default(),
            body: prompt,
        },
        cancel,
    )
    .await;
    match delivered {
        Delivered::Opened { url } | Delivered::AlreadyOpen { url } => {
            frames.append_event(EventKind::PullRequestOpened { url })?;
        }
        other => frames.append_output(&other.to_string())?,
    }
    Ok(())
}

/// How a round closes once delivery is over. The job reads as running until
/// its verdict, so a cancel can arrive after the round passed; it fails the
/// round, as it would have failed it any earlier, whether or not a pull
/// request was opened before it came.
fn closing_after_delivery(round: FinishedRound, cancel: &CancellationToken) -> FinishedRound {
    match (round.verdict(), cancel.is_cancelled()) {
        (Verdict::Passed, true) => {
            round.failed_because("cancelled while delivering its pull request".to_string())
        }
        (Verdict::Passed, false) | (Verdict::Failed, _) => round,
    }
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
        report: JobReport::from_events(payload.job_id, &frames.events_so_far()),
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
