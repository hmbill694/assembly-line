//! Starting a job and revising one: from what the user asked for, through
//! every check that can refuse it, to the round, its report and its delivery.
//!
//! Preparing allocates no job, so a mistake costs nothing and every reason
//! to refuse is known before a job directory exists. Running allocates, runs
//! the round and hands its branch on.

use crate::collect::{collect, record_launch_failure};
use crate::config::{self, ConfigError, RepoConfig, Warning};
use crate::delivery::{self, Delivered, PullRequestText};
use crate::event::{Event, EventKind, EventLog};
use crate::git::{self, PinnedRef};
use crate::job::JobOutcome;
use crate::paths::{self, JobMeta, JobPaths};
use crate::payload::{self, JobPayload, RoundRequest};
use crate::report::JobReport;
use crate::runner::{JobSecrets, Runner, RunnerProblem, reasons_a_container_cannot_run};
use crate::workspace::{DEFAULT_REMOTE, JOB_BRANCH_PATTERN, job_branch_name};
use anyhow::anyhow;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

/// What `assembly run` was asked for, as given on the command line.
#[derive(Debug, Clone)]
pub struct StartRequest {
    pub prompt: Option<String>,
    pub prompt_file: Option<PathBuf>,
    pub repo: Option<PathBuf>,
    pub base_ref: Option<String>,
    pub provider: Option<String>,
}

/// What `assembly revise` was asked for, as given on the command line.
#[derive(Debug, Clone)]
pub struct RevisionRequest {
    pub job_id: u64,
    pub feedback: String,
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
    meta: JobMeta,
    destination: Destination,
    start: PinnedRef,
    /// Where the job clones from and pushes to — `DEFAULT_REMOTE`'s URL.
    remote_url: String,
    round_prompt: String,
    config: RepoConfig,
    secrets: JobSecrets,
}

enum Destination {
    NewJob {
        remote_job_branches: Vec<String>,
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
    pub outcome: JobOutcome,
    /// `None` when the job's own event log could not be read back.
    pub report: Option<JobReport>,
    pub handoff: Handoff,
    started_the_job: bool,
}

/// What became of a finished round's branch.
#[derive(Debug)]
pub enum Handoff {
    /// The agent changed nothing, so there is nothing to deliver.
    NoBranch,
    /// A failed job still leaves a real branch, but opening a pull request
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
                    "branch: {branch} (not delivered — the job did not pass)"
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

/// Everything `assembly run` checks before a job directory is allocated.
pub async fn prepare_start<'r, R: Runner>(
    runner: &'r R,
    pass_env: &[String],
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
    let remote_job_branches = match git::remote_branches_matching(
        &located.repo,
        DEFAULT_REMOTE,
        JOB_BRANCH_PATTERN,
    )
    .await
    {
        Ok(branches) => branches,
        Err(e) => return Prepared::refused(notes, Refusal::Unpreparable(e)),
    };
    let secrets = match runnable_secrets(runner, &config, &located.remote_url, pass_env).await {
        Ok(secrets) => secrets,
        Err(refusal) => return Prepared::refused(notes, refusal),
    };

    Prepared {
        notes,
        round: Ok(ReadyRound {
            runner,
            meta: JobMeta {
                repo: located.repo,
                base_ref: located.base_ref,
                prompt: located.prompt.clone(),
                provider,
            },
            destination: Destination::NewJob {
                remote_job_branches,
            },
            start: located.start,
            remote_url: located.remote_url,
            round_prompt: located.prompt,
            config,
            secrets,
        }),
    }
}

/// Everything `assembly revise` checks before the job's next round runs.
pub async fn prepare_revision<'r, R: Runner>(
    runner: &'r R,
    pass_env: &[String],
    request: RevisionRequest,
) -> Prepared<'r, R> {
    let located = match locate_revision(request.job_id, request.repo).await {
        Ok(located) => located,
        Err(e) => return Prepared::refused(Vec::new(), Refusal::Unpreparable(e)),
    };
    let (notes, runnable) =
        runnable_config_and_provider(located.declared, Some(located.meta.provider.clone()));
    let config = match runnable {
        Ok((config, _)) => config,
        Err(refusal) => return Prepared::refused(notes, refusal),
    };
    let secrets = match runnable_secrets(runner, &config, &located.remote_url, pass_env).await {
        Ok(secrets) => secrets,
        Err(refusal) => return Prepared::refused(notes, refusal),
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
    let round = rounds_so_far(&located.events) + 1;
    let round_prompt = payload::revised_prompt(&located.meta.prompt, &request.feedback);

    Prepared {
        notes,
        round: Ok(ReadyRound {
            runner,
            meta: located.meta,
            destination: Destination::ExistingJob {
                paths: located.paths,
                round,
                log,
            },
            start: located.tip,
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
/// When the job directory cannot be allocated, or the round's event log
/// cannot be written.
pub async fn run<R: Runner>(
    ready: ReadyRound<'_, R>,
    cancel: CancellationToken,
) -> anyhow::Result<RoundConclusion> {
    let ReadyRound {
        runner,
        meta,
        destination,
        start,
        remote_url,
        round_prompt,
        config,
        secrets,
    } = ready;
    let started_the_job = matches!(destination, Destination::NewJob { .. });
    let (paths, mut log, round) = match destination {
        Destination::NewJob {
            remote_job_branches,
        } => allocate_job(&meta, &remote_job_branches).map(|(paths, log)| (paths, log, 1))?,
        Destination::ExistingJob { paths, round, log } => (paths, log, round),
    };

    let payload = payload_for_runner::<R>(
        &config,
        RoundRequest {
            job_id: paths.id,
            round,
            prompt: &round_prompt,
            provider: &meta.provider,
            start,
            remote_name: DEFAULT_REMOTE,
            remote_url,
            // `copy` is declared by the repository, so its paths resolve
            // against the repository — not against wherever the user stands.
            seed_from: &meta.repo,
        },
    )?;
    let outcome = collect_round(runner, &payload, &secrets, &mut log, &paths.log(), cancel).await?;

    let report = events_of(&paths)
        .ok()
        .map(|events| JobReport::from_events(paths.id, &events));
    let branch = report.as_ref().and_then(|report| report.branch.clone());
    let handoff = hand_off(
        &meta.repo,
        &config,
        &meta.base_ref,
        branch,
        outcome,
        PullRequestText {
            title: &payload.commit_message,
            body: &meta.prompt,
        },
    )
    .await;

    Ok(RoundConclusion {
        job: paths,
        outcome,
        report,
        handoff,
        started_the_job,
    })
}

/// The job `job_id` names in `repo`, or the latest one there when it names
/// none.
fn locate_job(job_id: Option<u64>, repo: Option<PathBuf>) -> anyhow::Result<(JobPaths, JobMeta)> {
    let repo_root = repository_named_or_enclosing(repo)?;
    let jobs_root = paths::jobs_root(&repo_root);

    let id = match job_id {
        Some(id) => id,
        None => paths::latest_job_id(&jobs_root)?.ok_or_else(|| anyhow!("no jobs yet"))?,
    };

    let paths = paths::open_job(&jobs_root, id)?;
    let meta =
        paths::read_meta(&paths).map_err(|e| anyhow!("reading meta.json for job {id}: {e}"))?;
    Ok((paths, meta))
}

/// The report on the job `job_id` names in `repo`, or on the latest one there
/// when it names none.
///
/// # Errors
///
/// When the job cannot be found, or its event log cannot be read.
pub fn report_for_job(job_id: Option<u64>, repo: Option<PathBuf>) -> anyhow::Result<JobReport> {
    let (paths, _) = locate_job(job_id, repo)?;
    events_of(&paths).map(|events| JobReport::from_events(paths.id, &events))
}

/// Where job `job_id` in `repo` captured its output.
///
/// # Errors
///
/// When the job cannot be found, or has captured nothing yet.
pub fn output_log_of(job_id: u64, repo: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    let (paths, _) = locate_job(Some(job_id), repo)?;
    let log = paths.log();
    match log.exists() {
        true => Ok(log),
        false => Err(anyhow!("job {job_id} has captured no output yet")),
    }
}

fn events_of(paths: &JobPaths) -> anyhow::Result<Vec<Event>> {
    EventLog::read(paths.events()).map_err(|e| anyhow!("reading the event log: {e}"))
}

/// Which repository a command acts on: whatever `--repo` named, else the one
/// the user is standing in.
///
/// Every command resolves it the same way, so a job started with `--repo` is
/// findable by `status`, `logs` and `revise` with the same `--repo`.
fn repository_named_or_enclosing(repo: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    match repo {
        Some(named) => Ok(named),
        None => enclosing_repo_root(),
    }
}

/// Job state lives under the repo, so every command needs to know which repo.
fn enclosing_repo_root() -> anyhow::Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    paths::git_root(&cwd).ok_or_else(|| {
        anyhow!("not inside a git repository — assembly stores job state at <git-root>/.assembly")
    })
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
    let repo = repository_named_or_enclosing(repo)?;
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
    meta: JobMeta,
    events: Vec<Event>,
    remote_url: String,
    tip: PinnedRef,
    declared: RepoConfig,
}

async fn locate_revision(job_id: u64, repo: Option<PathBuf>) -> anyhow::Result<RevisionLocated> {
    let (paths, meta) = locate_job(Some(job_id), repo)?;
    let events = events_of(&paths)?;

    let remote_url = payload::remote_to_clone(&meta.repo, DEFAULT_REMOTE).await?;
    let base = git::pinned(&meta.repo, DEFAULT_REMOTE, &meta.base_ref).await?;
    let tip = job_branch_tip(&meta.repo, job_id).await?;
    // `base`, not the job's own branch: the previous round is not allowed to
    // have changed the settings that govern this one.
    let declared = RepoConfig::from_ref(&meta.repo, &base.sha).await?;

    Ok(RevisionLocated {
        paths,
        meta,
        events,
        remote_url,
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

/// The secrets the chosen runner will carry into the job, once nothing about
/// the runner stands in the way.
async fn runnable_secrets<R: Runner>(
    runner: &R,
    config: &RepoConfig,
    remote_url: &str,
    pass_env: &[String],
) -> Result<JobSecrets, Refusal> {
    let (secrets, missing) = match R::RUNS_IN_A_CONTAINER {
        true => JobSecrets::from_host_environment(pass_env),
        false => (JobSecrets::default(), Vec::new()),
    };
    let container = match R::RUNS_IN_A_CONTAINER {
        true => reasons_a_container_cannot_run(&config.copy, remote_url),
        false => Vec::new(),
    };
    let problems: Vec<RunnerProblem> = runner
        .reasons_it_cannot_run()
        .await
        .into_iter()
        .chain(container)
        .chain(missing)
        .collect();

    match problems.is_empty() {
        true => Ok(secrets),
        false => Err(Refusal::RunnerCannotRun(problems)),
    }
}

/// A new job's directory, its `meta.json` and its open event log.
///
/// Called only once the round is known good, so a repository that has not
/// opted in leaves no litter. Its id is past every job branch the remote
/// already carries, whoever's they are.
fn allocate_job(
    meta: &JobMeta,
    remote_job_branches: &[String],
) -> anyhow::Result<(JobPaths, EventLog)> {
    let jobs_root = paths::jobs_root(&meta.repo);

    let paths = paths::next_job_id(&jobs_root, remote_job_branches)
        .and_then(|id| paths::create_job(&jobs_root, id))
        .map_err(|e| anyhow!("preparing the job directory: {e}"))?;

    paths::write_meta(&paths, meta).map_err(|e| anyhow!("writing meta.json: {e}"))?;

    EventLog::open_append(paths.events())
        .map(|log| (paths, log))
        .map_err(|e| anyhow!("opening the event log: {e}"))
}

/// A round's payload, fitted to where it runs. A container has neither the
/// host's toolchain nor its SSH keys, so it provisions the one and reaches
/// the remote over HTTPS, with a token, in place of the other.
fn payload_for_runner<R: Runner>(
    config: &RepoConfig,
    request: RoundRequest<'_>,
) -> anyhow::Result<JobPayload> {
    let request = RoundRequest {
        remote_url: match R::RUNS_IN_A_CONTAINER {
            true => payload::https_equivalent(&request.remote_url),
            false => request.remote_url,
        },
        ..request
    };
    JobPayload::for_round(config, request).map(|payload| JobPayload {
        provision_toolchain: R::RUNS_IN_A_CONTAINER,
        ..payload
    })
}

/// Launch the round and collect it. A launch failure is a failed round, not
/// a usage error: the job directory already exists and must say what
/// became of it. `cancel` reaches the launch too, so Ctrl-C while a job is
/// still starting stops it rather than waiting for it to start.
async fn collect_round<R: Runner>(
    runner: &R,
    payload: &JobPayload,
    secrets: &JobSecrets,
    log: &mut EventLog,
    output_log: &Path,
    cancel: CancellationToken,
) -> anyhow::Result<JobOutcome> {
    match runner.launch(payload, secrets, &cancel).await {
        Ok(job) => collect(job, log, output_log, payload.round, cancel).await,
        Err(e) => record_launch_failure(log, payload.round, &e),
    }
}

/// Hand a finished job's branch on, once `verify` accepted it.
async fn hand_off(
    repo: &Path,
    config: &RepoConfig,
    base_ref: &str,
    branch: Option<String>,
    outcome: JobOutcome,
    pull_request: PullRequestText<'_>,
) -> Handoff {
    match (branch, outcome) {
        (None, _) => Handoff::NoBranch,
        (Some(branch), JobOutcome::Failed) => Handoff::Withheld { branch },
        (Some(branch), JobOutcome::Passed) => {
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

/// How many rounds this job has already had, so the next one is numbered.
fn rounds_so_far(events: &[Event]) -> u32 {
    u32::try_from(
        events
            .iter()
            .filter(|e| matches!(e.kind, EventKind::JobStarted { .. }))
            .count(),
    )
    .unwrap_or(u32::MAX)
}

/// Where a revise round starts: the remote's copy of the job's branch.
///
/// A job whose earlier rounds committed nothing pushed nothing, so its branch
/// is absent rather than unpushed, and "push it first" would be the wrong
/// advice.
async fn job_branch_tip(repo: &Path, job_id: u64) -> anyhow::Result<PinnedRef> {
    let branch = job_branch_name(job_id);
    match git::pinned(repo, DEFAULT_REMOTE, &branch).await {
        Ok(tip) => Ok(tip),
        Err(e) => match git::remote_lacks_ref(repo, DEFAULT_REMOTE, &branch).await {
            Ok(true) => Err(anyhow!(
                "job {job_id} has no branch on '{DEFAULT_REMOTE}' — its first round committed \
                 nothing, so there is nothing to revise"
            )),
            Ok(false) | Err(_) => Err(e),
        },
    }
}
