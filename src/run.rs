//! `assembly run`: one round, in this process — what every runner launches,
//! and what a person types to reproduce a round.
//!
//! It keeps no state. What it did is its output — frames for a collector, or
//! readable lines for a person — and the branch and pull request it leaves.
//! Preparing claims nothing until every check that can refuse the round has
//! passed, so a refusal costs no id.

use crate::admission::{self, AdmissionRefused};
use crate::claim::no_branch_for_job_on;
use crate::config::JobSettings;
use crate::delivery::{self, BaseDiffers, Delivered, DeliveryPlan, PullRequestText};
use crate::event::EventKind;
use crate::frame::FrameWriter;
use crate::git::PinnedRef;
use crate::job::JobId;
use crate::locate::NamedRepo;
use crate::payload::{RoundBeforeSettings, RoundPayload};
use crate::remote::Remote;
use crate::report::JobReport;
use crate::round::{FinishedRound, Verdict, run_round};
use crate::workspace::{self, ScratchClone};
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

/// Why a round will not run, decided before anything was claimed for it.
#[derive(Debug)]
pub enum RunRefused {
    NotAdmitted(AdmissionRefused),
    NoJobBranch(JobId, Box<Remote>),
    Unpreparable(anyhow::Error),
}

impl RunRefused {
    /// Every reason, one per line, to report ahead of the refusal itself.
    #[must_use]
    pub fn itemized_reasons(&self) -> Vec<String> {
        match self {
            Self::NotAdmitted(refused) => refused.itemized_reasons(),
            Self::NoJobBranch(..) | Self::Unpreparable(_) => Vec::new(),
        }
    }

    /// Why the round failed, as its verdict records it: the fault, without
    /// advice that only makes sense to whoever typed `run`.
    #[must_use]
    pub fn to_failure_reason(&self) -> String {
        match self {
            Self::NotAdmitted(refused) => refused.to_failure_reason(),
            Self::NoJobBranch(id, remote) => no_branch_for_job_on(remote, *id),
            Self::Unpreparable(e) => e.to_string(),
        }
    }
}

impl std::fmt::Display for RunRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAdmitted(refused) => write!(f, "{refused}"),
            Self::NoJobBranch(id, remote) => write!(
                f,
                "{}, so there is nothing to continue; run without --job to start a new job",
                no_branch_for_job_on(remote, *id)
            ),
            Self::Unpreparable(e) => write!(f, "{e}"),
        }
    }
}

/// A round that has passed every check: cloned, its config read and valid,
/// its job's id claimed or its branch found.
#[derive(Debug)]
pub struct AdmittedRound {
    clone: ScratchClone,
    payload: RoundPayload,
    settings: JobSettings,
    base: PinnedRef,
    prompt: String,
    claimed: bool,
    provisioning_deadline: Instant,
}

/// What a finished round left.
#[derive(Debug)]
pub struct RoundConclusion {
    pub job: JobId,
    pub verdict: Verdict,
    /// Folded from this round's frames alone: `run` never sees the job's
    /// earlier rounds.
    pub this_rounds_report: JobReport,
    pub base_differs: Option<BaseDiffers>,
}

impl RoundConclusion {
    /// What the round left, as printed once it is over. Unlike `status`, no
    /// `logs:` line: `run` keeps no state for `logs` to read.
    #[must_use]
    pub fn to_lines(&self) -> Vec<String> {
        self.base_differs
            .iter()
            .map(|differs| format!("note: {differs}"))
            .chain(std::iter::once(self.to_summary_line()))
            .chain(self.this_rounds_report.work_location_lines())
            .collect()
    }

    /// `job 1: round failed (1 file +1/-0) — verify rejected the work: exit 1`.
    /// `run` writes neither `RoundRequested` nor `RoundStarted`, so the
    /// report names no round.
    fn to_summary_line(&self) -> String {
        let verdict = match self.verdict {
            Verdict::Passed => "passed",
            Verdict::Failed => "failed",
        };
        format!(
            "job {}: round {verdict}{}",
            self.job,
            self.this_rounds_report.to_summary_after_label()
        )
    }
}

/// Everything `run` checks, and the claim, before the round.
///
/// # Errors
///
/// A [`RunRefused`] for every reason the round cannot run; nothing is
/// claimed on the remote for a refused round.
pub async fn prepare_run(
    request: RunRequest,
    scratch_root: &Path,
    cancel: &CancellationToken,
) -> Result<AdmittedRound, RunRefused> {
    let prompt =
        prompt_text(request.prompt, request.prompt_file).map_err(RunRefused::Unpreparable)?;
    let repo = NamedRepo::named_or_enclosing(request.repo)
        .await
        .map_err(RunRefused::Unpreparable)?;
    let base_ref = repo
        .ref_to_start_from(request.base_ref)
        .await
        .map_err(RunRefused::Unpreparable)?;
    let (ref_name, pinned_sha) = match base_ref.parse::<PinnedRef>() {
        Ok(PinnedRef { name, sha }) => (name, Some(sha)),
        Err(_) => (base_ref, None),
    };

    let (clone, provisioning_deadline) =
        workspace::clone_within_provisioning_limit(&repo.remote, scratch_root, cancel)
            .await
            .map_err(RunRefused::Unpreparable)?;

    let base = workspace::pin_in_clone(&clone, &ref_name, pinned_sha.as_ref())
        .await
        .map_err(RunRefused::Unpreparable)?;
    let admitted = admission::admit(clone.path(), base, request.provider.as_deref())
        .await
        .map_err(RunRefused::NotAdmitted)?;

    // Pinning and admitting change nothing on the remote; claiming does, so
    // a round cancelled by now claims nothing.
    if cancel.is_cancelled() {
        return Err(RunRefused::Unpreparable(anyhow!("cancelled")));
    }
    let (job_id, start, claimed) = match request.job.map(JobId::from) {
        Some(id) => (id, job_branch_tip(&clone, id).await?, false),
        None => (
            workspace::claim_job_in_clone(&clone, &admitted)
                .await
                .map_err(RunRefused::Unpreparable)?,
            admitted.base().clone(),
            true,
        ),
    };

    let (base, settings) = admitted.into_base_and_settings();
    let payload = RoundPayload::for_round(
        &settings,
        RoundBeforeSettings {
            job_id,
            prompt: &prompt,
            start,
            provision_toolchain: request.provision_toolchain,
        },
    );

    Ok(AdmittedRound {
        clone,
        payload,
        settings,
        base,
        prompt,
        claimed,
        provisioning_deadline,
    })
}

/// Where continuing job `id` starts: its branch's tip on the remote.
async fn job_branch_tip(clone: &ScratchClone, id: JobId) -> Result<PinnedRef, RunRefused> {
    workspace::job_branch_in_clone(clone, id)
        .await
        .map_err(RunRefused::Unpreparable)?
        .ok_or_else(|| RunRefused::NoJobBranch(id, Box::new(clone.remote().clone())))
}

impl AdmittedRound {
    /// Which job's round is about to run, and from where.
    #[must_use]
    pub fn to_announcement_line(&self) -> String {
        match self.claimed {
            true => format!(
                "job {}: claimed {} at {} ({})",
                self.payload.job_id,
                self.payload.job_id.branch_name(),
                self.base.name,
                self.base.sha.short()
            ),
            false => format!(
                "job {}: continuing {}",
                self.payload.job_id,
                self.payload.job_id.branch_name()
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
    ) -> anyhow::Result<RoundConclusion> {
        let AdmittedRound {
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
        payload.job_id,
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
) -> RoundConclusion {
    RoundConclusion {
        job: payload.job_id,
        verdict,
        this_rounds_report: JobReport::from_events(payload.job_id, &frames.events_so_far()),
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
