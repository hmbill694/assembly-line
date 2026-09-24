use assembly_line::cli::{Cli, Command, RunnerArgs, RunnerKind};
use assembly_line::collect::{collect, record_launch_failure};
use assembly_line::config::RepoConfig;
use assembly_line::event::{Event, EventKind, EventLog};
use assembly_line::frame::FrameWriter;
use assembly_line::job::{JobOutcome, run_round};
use assembly_line::paths::{JobMeta, JobPaths};
use assembly_line::payload::{self, JobPayload, PAYLOAD_VAR, RoundRequest};
use assembly_line::report::JobReport;
use assembly_line::runner::docker::DockerRunner;
use assembly_line::runner::local::LocalRunner;
use assembly_line::runner::{
    self, JobSecrets, Runner, RunnerProblem, reasons_a_container_cannot_run,
};
use assembly_line::workspace::{DEFAULT_REMOTE, job_branch_name};
use assembly_line::{config, delivery, git, paths};
use clap::Parser;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use tokio_util::sync::CancellationToken;

/// Reserved for the user's mistake — a repository that has not opted in, a
/// missing job, a wrong directory. A job that runs and fails exits 1 instead.
const EXIT_USAGE: u8 = 2;
const EXIT_JOB_FAILED: u8 = 1;

fn main() -> ExitCode {
    install_tracing();

    match Cli::parse().command {
        Command::Run {
            prompt,
            prompt_file,
            repo,
            base_ref,
            provider,
            runner,
        } => in_async_runtime(run_work_on_chosen_runner(
            runner,
            Work::Start {
                prompt,
                prompt_file,
                repo,
                base_ref,
                provider,
            },
        )),
        Command::Revise {
            job_id,
            feedback,
            repo,
            runner,
        } => in_async_runtime(run_work_on_chosen_runner(
            runner,
            Work::Revise {
                job_id,
                feedback,
                repo,
            },
        )),
        Command::Status { job_id, repo } => print_job_status(job_id, repo),
        Command::Logs {
            job_id,
            follow,
            repo,
        } => print_job_log(job_id, follow, repo),
        Command::JobExec => in_async_runtime(execute_payload_from_environment()),
    }
}

/// On stderr: stdout belongs to frames.
fn install_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "assembly_line=info".into()),
        )
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
}

/// `job-exec`: read the payload, run the round, report it on stdout.
///
/// The exit code mirrors the round, but the collector decides from the
/// frames — the code only matters when the frames never said.
async fn execute_payload_from_environment() -> Result<ExitCode, String> {
    let payload: JobPayload = std::env::var(PAYLOAD_VAR)
        .map_err(|_| format!("{PAYLOAD_VAR} is not set — job-exec is started by a runner"))
        .and_then(|json| {
            serde_json::from_str(&json)
                .map_err(|e| format!("{PAYLOAD_VAR} is not a job payload: {e}"))
        })?;

    let frames = FrameWriter::new(std::io::stdout());
    let cancel = CancellationToken::new();
    cancel_on_termination_signal(cancel.clone());

    run_round(&payload, &frames, &std::env::temp_dir(), cancel)
        .await
        .map(exit_code_for)
        .map_err(|e| e.to_string())
}

/// SIGTERM is how the local runner cancels, SIGINT is Ctrl-C reaching the
/// whole foreground process group, and SIGHUP is the terminal closing — which
/// the agent, leading a session of its own, never hears. Any of them cancels
/// the round, which then still reports itself.
fn cancel_on_termination_signal(cancel: CancellationToken) {
    use tokio::signal::unix::{SignalKind, signal};

    tokio::spawn(async move {
        let (Ok(mut terminate), Ok(mut hangup)) = (
            signal(SignalKind::terminate()),
            signal(SignalKind::hangup()),
        ) else {
            return;
        };
        tokio::select! {
            _ = terminate.recv() => {}
            _ = hangup.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
        cancel.cancel();
    });
}

/// What `run` and `revise` do, independent of where the round runs.
enum Work {
    Start {
        prompt: Option<String>,
        prompt_file: Option<PathBuf>,
        repo: Option<PathBuf>,
        base_ref: Option<String>,
        provider: Option<String>,
    },
    Revise {
        job_id: u64,
        feedback: String,
        repo: Option<PathBuf>,
    },
}

/// The one place flags become a concrete runner; everything after it is
/// generic over [`Runner`].
async fn run_work_on_chosen_runner(args: RunnerArgs, work: Work) -> Result<ExitCode, String> {
    match args.runner {
        RunnerKind::Local if args.image.is_some() || !args.pass_env.is_empty() => Err(
            "--image and --pass-env apply to container runners; the local runner uses your \
             machine as it is"
                .into(),
        ),
        RunnerKind::Local => {
            run_work(
                &LocalRunner::current_binary().map_err(|e| e.to_string())?,
                &[],
                work,
            )
            .await
        }
        RunnerKind::Docker => {
            let image = args.image.unwrap_or_else(runner::published_image);
            run_work(&DockerRunner::new(image), &args.pass_env, work).await
        }
    }
}

async fn run_work<R: Runner>(
    runner: &R,
    pass_env: &[String],
    work: Work,
) -> Result<ExitCode, String> {
    match work {
        Work::Start {
            prompt,
            prompt_file,
            repo,
            base_ref,
            provider,
        } => {
            start_new_job(
                runner,
                pass_env,
                prompt,
                prompt_file,
                repo,
                base_ref,
                provider,
            )
            .await
        }
        Work::Revise {
            job_id,
            feedback,
            repo,
        } => revise_existing_job(runner, pass_env, job_id, feedback, repo).await,
    }
}

/// Every reason the chosen runner cannot run this repository's job, printed
/// together, before anything is allocated.
async fn runnable_secrets<R: Runner>(
    runner: &R,
    config: &RepoConfig,
    pass_env: &[String],
) -> Result<JobSecrets, String> {
    let (secrets, missing) = match R::RUNS_IN_A_CONTAINER {
        true => JobSecrets::from_host_environment(pass_env),
        false => (JobSecrets::default(), Vec::new()),
    };
    let container = match R::RUNS_IN_A_CONTAINER {
        true => reasons_a_container_cannot_run(&config.copy),
        false => Vec::new(),
    };
    let problems: Vec<RunnerProblem> = runner
        .reasons_it_cannot_run()
        .await
        .into_iter()
        .chain(container)
        .chain(missing)
        .collect();

    match problems.as_slice() {
        [] => Ok(secrets),
        problems => {
            problems.iter().for_each(|p| eprintln!("error: {p}"));
            Err("the job cannot run on this runner".into())
        }
    }
}

/// A round's payload, fitted to where it runs. A container has neither the
/// host's toolchain nor its SSH keys, so it provisions the one and reaches
/// the remote over HTTPS, with a token, in place of the other.
fn payload_for_runner<R: Runner>(
    config: &RepoConfig,
    request: RoundRequest<'_>,
) -> Result<JobPayload, String> {
    let request = RoundRequest {
        remote_url: match R::RUNS_IN_A_CONTAINER {
            true => payload::https_equivalent(&request.remote_url),
            false => request.remote_url,
        },
        ..request
    };
    JobPayload::for_round(config, request)
        .map(|payload| JobPayload {
            provision_toolchain: R::RUNS_IN_A_CONTAINER,
            ..payload
        })
        .map_err(|e| e.to_string())
}

/// Launch the round and collect it. A launch failure is a failed round, not
/// a usage error: the job directory already exists and must say what
/// became of it.
async fn collect_round<R: Runner>(
    runner: &R,
    payload: &JobPayload,
    secrets: &JobSecrets,
    log: &mut EventLog,
    output_log: &Path,
    cancel: CancellationToken,
) -> anyhow::Result<JobOutcome> {
    match runner.launch(payload, secrets).await {
        Ok(job) => collect(job, log, output_log, payload.round, cancel).await,
        Err(e) => record_launch_failure(log, payload.round, &e),
    }
}

/// Where every async command's usage error is reported, so none of them has to
/// unwind one by hand.
fn in_async_runtime(work: impl Future<Output = Result<ExitCode, String>>) -> ExitCode {
    match tokio::runtime::Runtime::new() {
        Ok(runtime) => match runtime.block_on(work) {
            Ok(code) => code,
            Err(e) => fail_with_usage_error(e),
        },
        Err(e) => fail_with_usage_error(format!("starting the async runtime: {e}")),
    }
}

fn fail_with_usage_error(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("error: {message}");
    ExitCode::from(EXIT_USAGE)
}

/// Job state lives under the repo, so every command needs to know which repo.
fn enclosing_repo_root() -> Result<PathBuf, String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    paths::git_root(&cwd).ok_or_else(|| {
        "not inside a git repository — assembly stores job state at <git-root>/.assembly".into()
    })
}

/// Which repository a command acts on: whatever `--repo` named, else the one
/// the user is standing in.
///
/// Every command resolves it the same way, so a job started with `--repo` is
/// findable by `status`, `logs` and `revise` with the same `--repo`.
fn repository_named_or_enclosing(repo: Option<PathBuf>) -> Result<PathBuf, String> {
    match repo {
        Some(named) => Ok(named),
        None => enclosing_repo_root(),
    }
}

/// A token that Ctrl-C cancels, for the round about to run.
fn cancel_on_ctrl_c() -> CancellationToken {
    let cancel = CancellationToken::new();
    let on_interrupt = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("\ninterrupted — cancelling the running agent");
            on_interrupt.cancel();
        }
    });
    cancel
}

fn prompt_text(prompt: Option<String>, prompt_file: Option<PathBuf>) -> Result<String, String> {
    match (prompt, prompt_file) {
        (Some(text), _) => Ok(text),
        (None, Some(path)) => {
            std::fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))
        }
        // clap refuses this combination before we are reached.
        (None, None) => Err("a job needs --prompt or --prompt-file".into()),
    }
}

/// What a job is cut from when the command line does not say: the branch the
/// repository has checked out, as the remote has it.
async fn default_base_ref(repo: &Path) -> Result<String, String> {
    match git::current_branch(repo).await {
        Ok(Some(branch)) => Ok(branch),
        Ok(None) => Err("HEAD is detached — name the ref to start from with --ref".into()),
        Err(e) => Err(e.to_string()),
    }
}

/// A job starts from the remote's copy of a ref. When the user's own copy
/// differs — usually unpushed commits — say so, rather than let them wonder
/// where their work went.
async fn note_if_local_ref_differs(repo: &Path, base_ref: &str, start: &git::PinnedRef) {
    if let Ok(local) = git::sha_at_ref(repo, base_ref).await
        && local != start.sha
    {
        println!(
            "note: your '{base_ref}' is not what '{DEFAULT_REMOTE}' has — the job starts \
             from {DEFAULT_REMOTE}'s ({}); push first if you meant yours",
            &start.sha[..12.min(start.sha.len())]
        );
    }
}

/// The repository's settings and the provider the job will use, once the two
/// are known to work together.
///
/// Warnings and every reason it cannot run are printed from here: deciding is
/// [`RepoConfig`]'s job, and saying so is this layer's.
fn runnable_config_and_provider(
    config: RepoConfig,
    chosen: Option<String>,
) -> Result<(RepoConfig, String), String> {
    let provider = chosen
        .or_else(|| config.provider.clone())
        .unwrap_or_default();

    config
        .settings_worth_flagging()
        .iter()
        .for_each(|w| eprintln!("warn: {w}"));

    let problems = config.reasons_it_cannot_run(&provider);
    match problems.as_slice() {
        [] => Ok((config, provider)),
        problems => {
            problems.iter().for_each(|e| eprintln!("error: {e}"));
            Err(format!("{} is not runnable", config::REPO_CONFIG_PATH))
        }
    }
}

async fn start_new_job<R: Runner>(
    runner: &R,
    pass_env: &[String],
    prompt: Option<String>,
    prompt_file: Option<PathBuf>,
    repo: Option<PathBuf>,
    base_ref: Option<String>,
    provider: Option<String>,
) -> Result<ExitCode, String> {
    let PreparedJob {
        repo,
        base_ref,
        start,
        remote_url,
        prompt,
        provider,
        config,
    } = prepare_job(prompt, prompt_file, repo, base_ref, provider).await?;
    let secrets = runnable_secrets(runner, &config, pass_env).await?;

    let meta = JobMeta {
        repo: repo.clone(),
        base_ref: base_ref.clone(),
        prompt: prompt.clone(),
        provider: provider.clone(),
    };
    let (paths, mut log) = allocate_job(&meta)?;

    let payload = payload_for_runner::<R>(
        &config,
        RoundRequest {
            job_id: paths.id,
            round: 1,
            prompt: &prompt,
            provider: &provider,
            start,
            remote_name: DEFAULT_REMOTE,
            remote_url,
            // `copy` is declared by the repository, so its paths resolve
            // against the repository — not against wherever the user stands.
            seed_from: &repo,
        },
    )?;
    let outcome = collect_round(
        runner,
        &payload,
        &secrets,
        &mut log,
        &paths.log(),
        cancel_on_ctrl_c(),
    )
    .await
    .map_err(|e| e.to_string())?;

    let code = finish_job(
        &repo,
        &config,
        &base_ref,
        &paths,
        outcome,
        delivery::PullRequestText {
            title: &payload.commit_message,
            body: &prompt,
        },
    )
    .await;
    println!("state: {}", paths.dir.display());
    Ok(code)
}

/// A new job's directory, its `meta.json` and its open event log.
///
/// Allocated only once the config is known good, so a repository that has not
/// opted in leaves no litter.
fn allocate_job(meta: &JobMeta) -> Result<(JobPaths, EventLog), String> {
    let jobs_root = paths::jobs_root(&meta.repo);

    let paths = paths::next_job_id(&jobs_root)
        .and_then(|id| paths::create_job(&jobs_root, id))
        .map_err(|e| format!("preparing the job directory: {e}"))?;

    paths::write_meta(&paths, meta).map_err(|e| format!("writing meta.json: {e}"))?;

    EventLog::open_append(paths.events())
        .map(|log| (paths, log))
        .map_err(|e| format!("opening the event log: {e}"))
}

/// What both `run` and `revise` do once the round is over: say what it left,
/// hand its branch on, and settle the exit code.
async fn finish_job(
    repo: &Path,
    config: &RepoConfig,
    base_ref: &str,
    paths: &JobPaths,
    outcome: JobOutcome,
    pull_request: delivery::PullRequestText<'_>,
) -> ExitCode {
    let branch = report_and_branch(paths);
    deliver_if_verified(
        repo,
        config,
        base_ref,
        branch.as_deref(),
        outcome,
        pull_request,
    )
    .await;
    exit_code_for(outcome)
}

/// Everything a job needs before a directory is allocated for it, so a
/// mistake costs nothing.
///
/// Named fields rather than a tuple: `base_ref`, `prompt` and `provider` are
/// all `String`, and a positional destructuring that transposed two of them
/// would compile and quietly send the prompt to git as a ref.
struct PreparedJob {
    repo: PathBuf,
    base_ref: String,
    /// `base_ref` as the remote has it — what the config was read from and
    /// what the job starts from.
    start: git::PinnedRef,
    /// Where the job clones from and pushes to — `DEFAULT_REMOTE`'s URL.
    remote_url: String,
    prompt: String,
    provider: String,
    config: RepoConfig,
}

async fn prepare_job(
    prompt: Option<String>,
    prompt_file: Option<PathBuf>,
    repo: Option<PathBuf>,
    base_ref: Option<String>,
    provider: Option<String>,
) -> Result<PreparedJob, String> {
    let prompt = prompt_text(prompt, prompt_file)?;
    let repo = repository_named_or_enclosing(repo)?;
    let base_ref = match base_ref {
        Some(named) => named,
        None => default_base_ref(&repo).await?,
    };

    // Before pinning, so a missing remote is reported as missing rather than
    // as a ref that is not on it.
    let remote_url = payload::remote_to_clone(&repo, DEFAULT_REMOTE)
        .await
        .map_err(|e| e.to_string())?;
    let start = git::pinned(&repo, DEFAULT_REMOTE, &base_ref)
        .await
        .map_err(|e| e.to_string())?;
    note_if_local_ref_differs(&repo, &base_ref, &start).await;

    let declared = RepoConfig::from_ref(&repo, &start.sha)
        .await
        .map_err(|e| e.to_string())?;
    let (config, provider) = runnable_config_and_provider(declared, provider)?;

    Ok(PreparedJob {
        repo,
        base_ref,
        start,
        remote_url,
        prompt,
        provider,
        config,
    })
}

fn exit_code_for(outcome: JobOutcome) -> ExitCode {
    match outcome {
        JobOutcome::Failed => ExitCode::from(EXIT_JOB_FAILED),
        JobOutcome::Passed => ExitCode::SUCCESS,
    }
}

fn events_of(paths: &JobPaths) -> Result<Vec<Event>, String> {
    EventLog::read(paths.events()).map_err(|e| format!("reading the event log: {e}"))
}

/// Print what the job's own log says became of it, and hand back the branch
/// it left — `None` when the agent changed nothing, so there is nothing to
/// deliver.
fn report_and_branch(paths: &JobPaths) -> Option<String> {
    let events = events_of(paths).ok()?;
    let report = JobReport::from_events(paths.id, &events);
    println!("{}", report.to_summary_line());
    report.branch
}

/// Hand a finished job's branch on, once `verify` accepted it.
///
/// A failed job still leaves a real branch, but opening a pull request for
/// work that did not pass is noise — the branch name is printed instead.
async fn deliver_if_verified(
    repo: &Path,
    config: &RepoConfig,
    base_ref: &str,
    branch: Option<&str>,
    outcome: JobOutcome,
    pull_request: delivery::PullRequestText<'_>,
) {
    let Some(branch) = branch else {
        return;
    };

    if outcome == JobOutcome::Failed {
        println!("branch: {branch} (not delivered — the job did not pass)");
        return;
    }

    let base = config.base.as_deref().unwrap_or(base_ref);

    if base != base_ref {
        println!(
            "note: this pull request will target '{base}', but the job was cut from \
             '{base_ref}' — review the diff before merging, since it carries everything \
             separating the two, not just this job's work"
        );
    }

    println!(
        "{}",
        delivery::deliver(repo, &config.delivery, branch, base, pull_request).await
    );
}

fn locate_job(job_id: Option<u64>, repo: Option<PathBuf>) -> Result<(JobPaths, JobMeta), String> {
    let repo_root = repository_named_or_enclosing(repo)?;
    let jobs_root = paths::jobs_root(&repo_root);

    let id = match job_id {
        Some(id) => id,
        None => paths::latest_job_id(&jobs_root)
            .map_err(|e| e.to_string())?
            .ok_or("no jobs yet")?,
    };

    let paths = paths::open_job(&jobs_root, id).map_err(|e| e.to_string())?;
    let meta =
        paths::read_meta(&paths).map_err(|e| format!("reading meta.json for job {id}: {e}"))?;
    Ok((paths, meta))
}

fn print_job_status(job_id: Option<u64>, repo: Option<PathBuf>) -> ExitCode {
    let lines = locate_job(job_id, repo).and_then(|(paths, _)| {
        events_of(&paths).map(|events| JobReport::from_events(paths.id, &events))
    });

    match lines {
        Ok(report) => {
            println!("{}", report.to_summary_line());
            if let Some(took) = report.to_duration_line() {
                println!("{took}");
            }
            if let Some(branch) = &report.branch {
                println!("branch: {branch}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => fail_with_usage_error(e),
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
async fn job_branch_tip(repo: &Path, job_id: u64) -> Result<git::PinnedRef, String> {
    let branch = job_branch_name(job_id);
    match git::pinned(repo, DEFAULT_REMOTE, &branch).await {
        Ok(tip) => Ok(tip),
        Err(e) => match git::remote_lacks_ref(repo, DEFAULT_REMOTE, &branch).await {
            Ok(true) => Err(format!(
                "job {job_id} has no branch on '{DEFAULT_REMOTE}' — its first round committed \
                 nothing, so there is nothing to revise"
            )),
            Ok(false) | Err(_) => Err(e.to_string()),
        },
    }
}

/// Run a job again with feedback. The round appends to the job's branch.
async fn revise_existing_job<R: Runner>(
    runner: &R,
    pass_env: &[String],
    job_id: u64,
    feedback: String,
    repo: Option<PathBuf>,
) -> Result<ExitCode, String> {
    let (paths, meta) = locate_job(Some(job_id), repo)?;
    let events = events_of(&paths)?;

    let remote_url = payload::remote_to_clone(&meta.repo, DEFAULT_REMOTE)
        .await
        .map_err(|e| e.to_string())?;
    let base = git::pinned(&meta.repo, DEFAULT_REMOTE, &meta.base_ref)
        .await
        .map_err(|e| e.to_string())?;
    let tip = job_branch_tip(&meta.repo, job_id).await?;
    // `base`, not the job's own branch: the previous round is not allowed to
    // have changed the settings that govern this one.
    let declared = RepoConfig::from_ref(&meta.repo, &base.sha)
        .await
        .map_err(|e| e.to_string())?;
    let (config, _) = runnable_config_and_provider(declared, Some(meta.provider.clone()))?;
    let secrets = runnable_secrets(runner, &config, pass_env).await?;

    let mut log =
        EventLog::open_append(paths.events()).map_err(|e| format!("opening the event log: {e}"))?;

    let round = rounds_so_far(&events) + 1;
    println!("revising job {job_id} (round {round})");

    let payload = payload_for_runner::<R>(
        &config,
        RoundRequest {
            job_id,
            round,
            prompt: &payload::revised_prompt(&meta.prompt, &feedback),
            provider: &meta.provider,
            start: tip,
            remote_name: DEFAULT_REMOTE,
            remote_url,
            seed_from: &meta.repo,
        },
    )?;
    let outcome = collect_round(
        runner,
        &payload,
        &secrets,
        &mut log,
        &paths.log(),
        cancel_on_ctrl_c(),
    )
    .await
    .map_err(|e| e.to_string())?;

    Ok(finish_job(
        &meta.repo,
        &config,
        &meta.base_ref,
        &paths,
        outcome,
        delivery::PullRequestText {
            title: &payload.commit_message,
            body: &meta.prompt,
        },
    )
    .await)
}

fn print_job_log(job_id: u64, follow: bool, repo: Option<PathBuf>) -> ExitCode {
    let paths = match locate_job(Some(job_id), repo) {
        Ok((paths, _)) => paths,
        Err(e) => return fail_with_usage_error(e),
    };

    let path = paths.log();
    if !path.exists() {
        return fail_with_usage_error(format!("job {job_id} has captured no output yet"));
    }

    match follow {
        // Delegated to `tail` rather than reimplemented; a machine without it
        // gets the spawn error.
        true => match std::process::Command::new("tail")
            .arg("-f")
            .arg(&path)
            .status()
        {
            Ok(_) => ExitCode::SUCCESS,
            Err(e) => fail_with_usage_error(e),
        },
        false => match std::fs::read_to_string(&path) {
            Ok(body) => {
                print!("{body}");
                ExitCode::SUCCESS
            }
            Err(e) => fail_with_usage_error(e),
        },
    }
}
