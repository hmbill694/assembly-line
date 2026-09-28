//! Starting a job and revising one: from what the user asked for, through
//! every check that can refuse it, to the round, its report and its delivery.
//!
//! Preparing allocates no job, so a mistake costs nothing and every reason
//! to refuse is known before a job directory exists. Running allocates, runs
//! the round and hands its branch on.

use crate::claim;
use crate::collect::{collect, record_launch_failure};
use crate::config::{self, ConfigError, RepoConfig, Warning};
use crate::delivery::{self, Delivered, PullRequestText};
use crate::event::{Event, EventKind, EventLog};
use crate::git::{self, PinnedRef};
use crate::job::JobId;
use crate::locate;
use crate::paths::{self, JobPaths, RepoKey};
use crate::payload::{self, RoundPayload, RoundRequest};
use crate::report::JobReport;
use crate::round::Verdict;
use crate::runner::{
    JobSecrets, Runner, RunnerProblem, payload_fitted_to, secrets_or_reasons_it_cannot_run,
};
use crate::workspace::DEFAULT_REMOTE;
use anyhow::anyhow;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

/// What `submit` was asked for: a new job, or another round of one.
#[derive(Debug, Clone)]
pub enum Work {
    Start(StartRequest),
    Revise(RevisionRequest),
}

impl Work {
    /// `submit`'s flags as the work they describe. clap has already kept
    /// `--ref` and `--provider` away from `--job`.
    #[must_use]
    pub fn from_submission(
        prompt: Option<String>,
        prompt_file: Option<PathBuf>,
        repo: Option<PathBuf>,
        base_ref: Option<String>,
        provider: Option<String>,
        job: Option<u64>,
    ) -> Work {
        match job {
            None => Work::Start(StartRequest {
                prompt,
                prompt_file,
                repo,
                base_ref,
                provider,
            }),
            Some(job_id) => Work::Revise(RevisionRequest {
                job_id,
                prompt,
                prompt_file,
                repo,
            }),
        }
    }
}

/// A new job, as `submit` was asked for it.
#[derive(Debug, Clone)]
pub struct StartRequest {
    pub prompt: Option<String>,
    pub prompt_file: Option<PathBuf>,
    pub repo: Option<PathBuf>,
    pub base_ref: Option<String>,
    pub provider: Option<String>,
}

/// Another round of job `job_id`, as `submit --job` was asked for it.
#[derive(Debug, Clone)]
pub struct RevisionRequest {
    pub job_id: u64,
    pub prompt: Option<String>,
    pub prompt_file: Option<PathBuf>,
    pub repo: Option<PathBuf>,
}

/// Something worth telling the user before the round starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    /// A job starts from the remote's copy of a ref. When the user's own copy
    /// differs — usually unpushed commits — they should hear so, rather than
    /// wonder where their work went.
    LocalRefDiffers {
        base_ref: String,
        remote: String,
        start_sha: String,
    },
    ConfigWarning(Warning),
}

impl std::fmt::Display for Note {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LocalRefDiffers {
                base_ref,
                remote,
                start_sha,
            } => write!(
                f,
                "note: your '{base_ref}' is not what '{remote}' has — the job starts from \
                 {remote}'s ({}); push first if you meant yours",
                &start_sha[..12.min(start_sha.len())]
            ),
            Self::ConfigWarning(warning) => write!(f, "warn: {warning}"),
        }
    }
}

/// Why a job will not run, decided before anything was allocated for it.
#[derive(Debug)]
pub enum Refusal {
    ConfigNotRunnable(Vec<ConfigError>),
    RunnerCannotRun(Vec<RunnerProblem>),
    Unpreparable(anyhow::Error),
}

impl Refusal {
    /// Every reason, one per line, to report ahead of the refusal itself.
    #[must_use]
    pub fn itemized_reasons(&self) -> Vec<String> {
        match self {
            Self::ConfigNotRunnable(errors) => errors.iter().map(ToString::to_string).collect(),
            Self::RunnerCannotRun(problems) => problems.iter().map(ToString::to_string).collect(),
            Self::Unpreparable(_) => Vec::new(),
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConfigNotRunnable(_) => write!(f, "{} is not runnable", config::REPO_CONFIG_PATH),
            Self::RunnerCannotRun(_) => write!(f, "the job cannot run on this runner"),
            Self::Unpreparable(e) => write!(f, "{e}"),
        }
    }
}

/// A prepared round, or why there is none, with whatever was worth saying on
/// the way — a refused round still has its notes.
pub struct Prepared<'r, R> {
    pub notes: Vec<Note>,
    pub round: Result<ReadyRound<'r, R>, Refusal>,
}

impl<R> Prepared<'_, R> {
    fn refused(notes: Vec<Note>, refusal: Refusal) -> Self {
        Prepared {
            notes,
            round: Err(refusal),
        }
    }
}

/// A round that has passed every check, bound to the runner that checked it.
pub struct ReadyRound<'r, R> {
    runner: &'r R,
    /// The checkout git runs in on the host: claiming, delivering.
    repo: PathBuf,
    base_ref: String,
    provider: String,
    /// What the job was first asked to do, which its pull request describes.
    original_prompt: String,
    destination: Destination,
    /// The commit the round starts from: the base for a new job, the job
    /// branch's tip for a revise.
    start: PinnedRef,
    /// The base as it is now, which the round's request records.
    requested_base: PinnedRef,
    /// Where the job clones from and pushes to — `DEFAULT_REMOTE`'s URL.
    remote_url: String,
    round_prompt: String,
    config: RepoConfig,
    secrets: JobSecrets,
}

enum Destination {
    NewJob {
        jobs_dir: PathBuf,
    },
    ExistingJob {
        paths: JobPaths,
        round: u32,
        log: EventLog,
    },
}

impl<R> ReadyRound<'_, R> {
    /// Which job and round are about to run, for a round that is not its
    /// job's first.
    #[must_use]
    pub fn to_announcement_line(&self) -> Option<String> {
        match &self.destination {
            Destination::NewJob { .. } => None,
            Destination::ExistingJob { paths, round, .. } => {
                Some(format!("revising job {} (round {round})", paths.id))
            }
        }
    }
}

#[derive(Debug)]
pub struct RoundConclusion {
    pub job: JobPaths,
    pub verdict: Verdict,
    /// `None` when the job's own event log could not be read back.
    pub report: Option<JobReport>,
    pub handoff: Handoff,
    started_the_job: bool,
}

/// What became of a finished round's branch.
#[derive(Debug)]
pub enum Handoff {
    /// The round pushed nothing — its branch, if any, holds no work of its
    /// own — so there is nothing to deliver.
    NoBranch,
    /// A failed round still leaves a real branch, but opening a pull request
    /// for work that did not pass is noise.
    Withheld { branch: String },
    Delivered {
        base_differs: Option<BaseDiffers>,
        delivered: Delivered,
    },
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

impl RoundConclusion {
    /// What the round left, as printed once it is over.
    #[must_use]
    pub fn to_lines(&self) -> Vec<String> {
        let summary = self.report.as_ref().map(JobReport::to_summary_line);
        let handoff: Vec<String> = match &self.handoff {
            Handoff::NoBranch => Vec::new(),
            Handoff::Withheld { branch } => {
                vec![format!(
                    "branch: {branch} (not delivered — the round did not pass)"
                )]
            }
            Handoff::Delivered {
                base_differs,
                delivered,
            } => base_differs
                .iter()
                .map(|differs| format!("note: {differs}"))
                .chain(std::iter::once(delivered.to_string()))
                .collect(),
        };
        let state = self
            .started_the_job
            .then(|| format!("state: {}", self.job.dir.display()));

        summary.into_iter().chain(handoff).chain(state).collect()
    }
}

/// Everything `submit` checks before a job directory is allocated.
pub async fn prepare_start<'r, R: Runner>(
    runner: &'r R,
    pass_env: &[String],
    root: &Path,
    request: StartRequest,
) -> Prepared<'r, R> {
    let located = match locate_start(
        request.prompt,
        request.prompt_file,
        request.repo,
        request.base_ref,
    )
    .await
    {
        Ok(located) => located,
        Err(e) => return Prepared::refused(Vec::new(), Refusal::Unpreparable(e)),
    };
    let jobs_dir = match RepoKey::from_remote_url(&located.remote_url) {
        Ok(key) => key.jobs_dir(root),
        Err(e) => return Prepared::refused(Vec::new(), Refusal::Unpreparable(e.into())),
    };
    let local_ref_note = local_ref_differs(&located.repo, &located.base_ref, &located.start).await;

    let declared = match RepoConfig::from_ref(&located.repo, &located.start.sha).await {
        Ok(declared) => declared,
        Err(e) => {
            return Prepared::refused(
                local_ref_note.into_iter().collect(),
                Refusal::Unpreparable(e),
            );
        }
    };
    let (warnings, runnable) = runnable_config_and_provider(declared, request.provider);
    let notes: Vec<Note> = local_ref_note.into_iter().chain(warnings).collect();
    let (config, provider) = match runnable {
        Ok(runnable) => runnable,
        Err(refusal) => return Prepared::refused(notes, refusal),
    };
    let secrets =
        match secrets_or_reasons_it_cannot_run(runner, &located.remote_url, pass_env, |name| {
            std::env::var(name).ok()
        })
        .await
        {
            Ok(secrets) => secrets,
            Err(problems) => return Prepared::refused(notes, Refusal::RunnerCannotRun(problems)),
        };

    Prepared {
        notes,
        round: Ok(ReadyRound {
            runner,
            repo: located.repo,
            base_ref: located.base_ref,
            provider,
            original_prompt: located.prompt.clone(),
            destination: Destination::NewJob { jobs_dir },
            requested_base: located.start.clone(),
            start: located.start,
            remote_url: located.remote_url,
            round_prompt: located.prompt,
            config,
            secrets,
        }),
    }
}

/// Everything `submit --job` checks before the job's next round runs.
pub async fn prepare_revision<'r, R: Runner>(
    runner: &'r R,
    pass_env: &[String],
    root: &Path,
    request: RevisionRequest,
) -> Prepared<'r, R> {
    let feedback = match prompt_text(request.prompt, request.prompt_file) {
        Ok(feedback) => feedback,
        Err(e) => return Prepared::refused(Vec::new(), Refusal::Unpreparable(e)),
    };
    let located = match locate_revision(root, request.job_id, request.repo).await {
        Ok(located) => located,
        Err(e) => return Prepared::refused(Vec::new(), Refusal::Unpreparable(e)),
    };
    let (notes, runnable) =
        runnable_config_and_provider(located.declared, Some(located.provider.clone()));
    let config = match runnable {
        Ok((config, _)) => config,
        Err(refusal) => return Prepared::refused(notes, refusal),
    };
    let secrets =
        match secrets_or_reasons_it_cannot_run(runner, &located.remote_url, pass_env, |name| {
            std::env::var(name).ok()
        })
        .await
        {
            Ok(secrets) => secrets,
            Err(problems) => return Prepared::refused(notes, Refusal::RunnerCannotRun(problems)),
        };
    let log = match EventLog::open_append(located.paths.events()) {
        Ok(log) => log,
        Err(e) => {
            return Prepared::refused(
                notes,
                Refusal::Unpreparable(anyhow!("opening the event log: {e}")),
            );
        }
    };
    let round = located.rounds + 1;
    let round_prompt = payload::revised_prompt(&located.original_prompt, &feedback);

    Prepared {
        notes,
        round: Ok(ReadyRound {
            runner,
            repo: located.checkout,
            base_ref: located.base.name.clone(),
            provider: located.provider,
            original_prompt: located.original_prompt,
            destination: Destination::ExistingJob {
                paths: located.paths,
                round,
                log,
            },
            start: located.tip,
            requested_base: located.base,
            remote_url: located.remote_url,
            round_prompt,
            config,
            secrets,
        }),
    }
}

/// Run a prepared round: allocate its job if it is a new one, launch and
/// collect the round, and hand its branch on once `verify` accepted it.
///
/// # Errors
///
/// When a new job cannot claim its id on the remote, its directory cannot be
/// allocated, or the round's event log cannot be written.
pub async fn run<R: Runner>(
    ready: ReadyRound<'_, R>,
    cancel: CancellationToken,
) -> anyhow::Result<RoundConclusion> {
    let ReadyRound {
        runner,
        repo,
        base_ref,
        provider,
        original_prompt,
        destination,
        start,
        requested_base,
        remote_url,
        round_prompt,
        config,
        secrets,
    } = ready;
    let started_the_job = matches!(destination, Destination::NewJob { .. });
    let (paths, mut log, round) = match destination {
        Destination::NewJob { jobs_dir } => allocate_job(&jobs_dir, &repo, &start.sha)
            .await
            .map(|(paths, log)| (paths, log, 1))?,
        Destination::ExistingJob { paths, round, log } => (paths, log, round),
    };
    log.append(EventKind::RoundRequested {
        remote_url: remote_url.clone(),
        base: requested_base,
        prompt: round_prompt.clone(),
        provider: provider.clone(),
    })
    .map_err(|e| anyhow!("recording the round's request: {e}"))?;

    let payload = RoundPayload::for_round(
        &config,
        RoundRequest {
            job_id: paths.id,
            round,
            prompt: &round_prompt,
            provider: &provider,
            start,
            remote_name: DEFAULT_REMOTE,
            remote_url,
        },
    )
    .map(payload_fitted_to::<R>)?;
    let verdict = collect_round(runner, &payload, &secrets, &mut log, &paths.log(), cancel).await?;

    let report = events_of(&paths)
        .ok()
        .map(|events| JobReport::from_events(paths.id.into(), &events));
    let branch = report.as_ref().and_then(|report| report.branch.clone());
    let handoff = hand_off(
        &repo,
        &config,
        &base_ref,
        branch,
        verdict,
        PullRequestText {
            title: payload.commit_message.lines().next().unwrap_or_default(),
            body: &original_prompt,
        },
    )
    .await;

    Ok(RoundConclusion {
        job: paths,
        verdict,
        report,
        handoff,
        started_the_job,
    })
}

/// The report on the job `job_id` names in `repo`, or on the latest one there
/// when it names none.
///
/// # Errors
///
/// When the job cannot be found, or its event log cannot be read.
pub async fn report_for_job(
    root: &Path,
    job_id: Option<u64>,
    repo: Option<PathBuf>,
) -> anyhow::Result<JobReport> {
    let paths = locate::job_at(root, repo, job_id).await?;
    events_of(&paths).map(|events| JobReport::from_events(paths.id.into(), &events))
}

/// Where job `job_id` in `repo` captured its output.
///
/// # Errors
///
/// When the job cannot be found, or has captured nothing yet.
pub async fn output_log_of(
    root: &Path,
    job_id: u64,
    repo: Option<PathBuf>,
) -> anyhow::Result<PathBuf> {
    let paths = locate::job_at(root, repo, Some(job_id)).await?;
    let log = paths.log();
    match log.exists() {
        true => Ok(log),
        false => Err(anyhow!("job {job_id} has captured no output yet")),
    }
}

fn events_of(paths: &JobPaths) -> anyhow::Result<Vec<Event>> {
    EventLog::read(paths.events()).map_err(|e| anyhow!("reading the event log: {e}"))
}

/// A new job's repository, prompt and start, before its config is read.
struct StartLocated {
    repo: PathBuf,
    base_ref: String,
    /// `base_ref` as the remote has it — what the config is read from and
    /// what the job starts from.
    start: PinnedRef,
    remote_url: String,
    prompt: String,
}

async fn locate_start(
    prompt: Option<String>,
    prompt_file: Option<PathBuf>,
    repo: Option<PathBuf>,
    base_ref: Option<String>,
) -> anyhow::Result<StartLocated> {
    let prompt = prompt_text(prompt, prompt_file)?;
    let repo = locate::checkout_named_or_enclosing(repo)?;
    let base_ref = match base_ref {
        Some(named) => named,
        None => default_base_ref(&repo).await?,
    };

    // Before pinning, so a missing remote is reported as missing rather than
    // as a ref that is not on it.
    let remote_url = payload::remote_to_clone(&repo, DEFAULT_REMOTE).await?;
    let start = git::pinned(&repo, DEFAULT_REMOTE, &base_ref).await?;

    Ok(StartLocated {
        repo,
        base_ref,
        start,
        remote_url,
        prompt,
    })
}

/// An existing job, the commit its next round starts from, and the config
/// that governs it.
struct RevisionLocated {
    paths: JobPaths,
    checkout: PathBuf,
    rounds: u32,
    remote_url: String,
    /// The job's base, pinned at the remote's tip now.
    base: PinnedRef,
    provider: String,
    original_prompt: String,
    tip: PinnedRef,
    declared: RepoConfig,
}

async fn locate_revision(
    root: &Path,
    job_id: u64,
    repo: Option<PathBuf>,
) -> anyhow::Result<RevisionLocated> {
    let checkout = locate::checkout_named_or_enclosing(repo)?;
    let (jobs_dir, remote_url) = locate::jobs_dir_of(root, Some(checkout.clone())).await?;
    let paths = paths::open_job(&jobs_dir, JobId::from(job_id))?;
    let report = JobReport::from_events(job_id, &events_of(&paths)?);
    let (Some(requested_base), Some(provider), Some(original_prompt)) =
        (report.base, report.provider, report.first_prompt)
    else {
        return Err(anyhow!(
            "job {job_id} has no recorded request — its first round never started, so there is \
             nothing to revise"
        ));
    };

    let base = git::pinned(&checkout, DEFAULT_REMOTE, &requested_base.name).await?;
    let tip = job_branch_tip(&checkout, paths.id).await?;
    // `base`, not the job's own branch: the previous round is not allowed to
    // have changed the settings that govern this one.
    let declared = RepoConfig::from_ref(&checkout, &base.sha).await?;

    Ok(RevisionLocated {
        paths,
        checkout,
        rounds: report.rounds,
        remote_url,
        base,
        provider,
        original_prompt,
        tip,
        declared,
    })
}

fn prompt_text(prompt: Option<String>, prompt_file: Option<PathBuf>) -> anyhow::Result<String> {
    match (prompt, prompt_file) {
        (Some(text), _) => Ok(text),
        (None, Some(path)) => {
            std::fs::read_to_string(&path).map_err(|e| anyhow!("reading {}: {e}", path.display()))
        }
        // clap refuses this combination before we are reached.
        (None, None) => Err(anyhow!("a job needs --prompt or --prompt-file")),
    }
}

/// What a job is cut from when the command line does not say: the branch the
/// repository has checked out, as the remote has it.
async fn default_base_ref(repo: &Path) -> anyhow::Result<String> {
    match git::current_branch(repo).await? {
        Some(branch) => Ok(branch),
        None => Err(anyhow!(
            "HEAD is detached — name the ref to start from with --ref"
        )),
    }
}

async fn local_ref_differs(repo: &Path, base_ref: &str, start: &PinnedRef) -> Option<Note> {
    let local = git::sha_at_ref(repo, base_ref).await.ok()?;
    (local != start.sha).then(|| Note::LocalRefDiffers {
        base_ref: base_ref.to_string(),
        remote: DEFAULT_REMOTE.to_string(),
        start_sha: start.sha.clone(),
    })
}

/// The repository's settings and the provider the job will use, once the two
/// are known to work together — with the settings worth flagging either way.
fn runnable_config_and_provider(
    config: RepoConfig,
    chosen: Option<String>,
) -> (Vec<Note>, Result<(RepoConfig, String), Refusal>) {
    let provider = chosen
        .or_else(|| config.provider.clone())
        .unwrap_or_default();
    let warnings = config
        .settings_worth_flagging()
        .into_iter()
        .map(Note::ConfigWarning)
        .collect();

    let problems = config.reasons_it_cannot_run(&provider);
    let runnable = match problems.is_empty() {
        true => Ok((config, provider)),
        false => Err(Refusal::ConfigNotRunnable(problems)),
    };
    (warnings, runnable)
}

/// A new job's directory under `jobs_dir` and its open event log, under the
/// id it claimed on `repo`'s remote.
///
/// Called only once the round is known good, so a repository that has not
/// opted in leaves no litter and burns no id.
async fn allocate_job(
    jobs_dir: &Path,
    repo: &Path,
    base_sha: &str,
) -> anyhow::Result<(JobPaths, EventLog)> {
    let id = claim::claim_job(repo, DEFAULT_REMOTE, base_sha).await?;
    let paths = paths::create_job(jobs_dir, id).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => anyhow!(
            "job {id}'s branch was gone from '{DEFAULT_REMOTE}', so its id was claimed again, \
             but {} still holds that job — submit again, and the next id will be claimed",
            jobs_dir.join(id.to_string()).display()
        ),
        _ => anyhow!("preparing the job directory: {e}"),
    })?;

    EventLog::open_append(paths.events())
        .map(|log| (paths, log))
        .map_err(|e| anyhow!("opening the event log: {e}"))
}

/// Launch the round and collect it. A launch failure is a failed round, not
/// a usage error: the job directory already exists and must say what
/// became of it. `cancel` reaches the launch too, so Ctrl-C while a round is
/// still starting stops it rather than waiting for it to start.
async fn collect_round<R: Runner>(
    runner: &R,
    payload: &RoundPayload,
    secrets: &JobSecrets,
    log: &mut EventLog,
    output_log: &Path,
    cancel: CancellationToken,
) -> anyhow::Result<Verdict> {
    match runner.launch(payload, secrets, &cancel).await {
        Ok(job) => collect(job, log, output_log, payload.round, cancel).await,
        Err(e) => record_launch_failure(log, payload.round, &e),
    }
}

/// Hand the job's branch on, once its round passed.
async fn hand_off(
    repo: &Path,
    config: &RepoConfig,
    base_ref: &str,
    branch: Option<String>,
    verdict: Verdict,
    pull_request: PullRequestText<'_>,
) -> Handoff {
    match (branch, verdict) {
        (None, _) => Handoff::NoBranch,
        (Some(branch), Verdict::Failed) => Handoff::Withheld { branch },
        (Some(branch), Verdict::Passed) => {
            let base = config.base.as_deref().unwrap_or(base_ref);
            let base_differs = (base != base_ref).then(|| BaseDiffers {
                base: base.to_string(),
                base_ref: base_ref.to_string(),
            });
            let delivered =
                delivery::deliver(repo, &config.delivery, &branch, base, pull_request).await;
            Handoff::Delivered {
                base_differs,
                delivered,
            }
        }
    }
}

/// Where a revise round starts: the remote's copy of the job's branch.
///
/// A job's branch can be absent rather than unpushed — deleted once its pull
/// request merged, or never pushed by a job from before ids were claimed on
/// the remote — and "push it first" would be the wrong advice.
async fn job_branch_tip(repo: &Path, job_id: JobId) -> anyhow::Result<PinnedRef> {
    let branch = job_id.branch_name();
    match git::pinned(repo, DEFAULT_REMOTE, &branch).await {
        Ok(tip) => Ok(tip),
        Err(e) => match git::remote_lacks_ref(repo, DEFAULT_REMOTE, &branch).await {
            Ok(true) => Err(anyhow!(
                "job {job_id} has no branch on '{DEFAULT_REMOTE}' — it was deleted, or the job \
                 never pushed one, so there is nothing to revise"
            )),
            Ok(false) | Err(_) => Err(e),
        },
    }
}
