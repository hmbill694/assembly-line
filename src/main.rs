use assembly_line::cli::{Cli, Command};
use assembly_line::config::RepoConfig;
use assembly_line::event::{Event, EventKind, EventLog};
use assembly_line::job::{JobOutcome, JobSpec, Revision, RunOpts, revise_job, run_job};
use assembly_line::paths::{JobMeta, JobPaths};
use assembly_line::report::JobReport;
use assembly_line::{config, delivery, gc, paths};
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
        } => in_async_runtime(start_new_job(prompt, prompt_file, repo, base_ref, provider)),
        Command::Revise {
            job_id,
            feedback,
            repo,
        } => in_async_runtime(revise_existing_job(job_id, feedback, repo)),
        Command::Status { job_id, repo } => print_job_status(job_id, repo),
        Command::Logs {
            job_id,
            follow,
            repo,
        } => print_job_log(job_id, follow, repo),
        Command::Gc {
            older_than,
            dry_run,
        } => in_async_runtime(remove_stale_worktrees(older_than, dry_run)),
    }
}

fn install_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "assembly_line=info".into()),
        )
        .with_target(false)
        .init();
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

fn cancel_on_ctrl_c(cancel: CancellationToken) {
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("\ninterrupted — cancelling the running agent");
            cancel.cancel();
        }
    });
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

/// What a job is cut from when the command line does not say: whatever the
/// repository has checked out, never an assumed `main`.
async fn default_base_ref(repo: &Path) -> Result<String, String> {
    assembly_line::git::current_branch(repo)
        .await
        .map_err(|e| e.to_string())
        .map(|branch| branch.unwrap_or_else(|| "HEAD".to_string()))
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

fn machine_opts(repo: &Path) -> RunOpts {
    let cancel = CancellationToken::new();
    cancel_on_ctrl_c(cancel.clone());

    RunOpts {
        cancel,
        repo: repo.to_path_buf(),
        // `copy` is declared by the repository, so its paths resolve against
        // the repository — not against wherever the user happened to stand.
        seed_from: repo.to_path_buf(),
        remote: assembly_line::workspace::DEFAULT_REMOTE.to_string(),
    }
}

async fn start_new_job(
    prompt: Option<String>,
    prompt_file: Option<PathBuf>,
    repo: Option<PathBuf>,
    base_ref: Option<String>,
    provider: Option<String>,
) -> Result<ExitCode, String> {
    let PreparedJob {
        repo,
        base_ref,
        prompt,
        provider,
        config,
    } = prepare_job(prompt, prompt_file, repo, base_ref, provider).await?;

    let meta = JobMeta {
        repo: repo.clone(),
        base_ref: base_ref.clone(),
        prompt: prompt.clone(),
        provider: provider.clone(),
    };
    let (paths, mut log) = allocate_job(&meta)?;

    let spec = JobSpec {
        prompt: &prompt,
        provider: &provider,
        base_ref: &base_ref,
        round: 1,
    };
    let outcome = run_job(&config, &spec, &paths, &mut log, &machine_opts(&repo))
        .await
        .map_err(|e| e.to_string())?;

    let code = finish_job(&repo, &config, &base_ref, &paths, outcome).await;
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
) -> ExitCode {
    let branch = report_and_branch(paths);
    deliver_if_verified(repo, config, base_ref, branch.as_deref(), outcome).await;
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

    let declared = RepoConfig::from_ref(&repo, &base_ref)
        .await
        .map_err(|e| e.to_string())?;
    let (config, provider) = runnable_config_and_provider(declared, provider)?;

    Ok(PreparedJob {
        repo,
        base_ref,
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

    match delivery::deliver(
        repo,
        &config.delivery,
        assembly_line::workspace::DEFAULT_REMOTE,
        branch,
        base,
    )
    .await
    {
        Ok(outcome) => println!("{outcome}"),
        Err(e) => eprintln!("warn: delivering {branch}: {e}"),
    }
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

/// Run a job again with feedback. The round appends to the job's branch.
async fn revise_existing_job(
    job_id: u64,
    feedback: String,
    repo: Option<PathBuf>,
) -> Result<ExitCode, String> {
    let (paths, meta) = locate_job(Some(job_id), repo)?;
    let events = events_of(&paths)?;

    // `base_ref`, not the job's own branch: the previous round is not allowed
    // to have changed the settings that govern this one.
    let declared = RepoConfig::from_ref(&meta.repo, &meta.base_ref)
        .await
        .map_err(|e| e.to_string())?;
    let (config, _) = runnable_config_and_provider(declared, Some(meta.provider.clone()))?;

    let mut log =
        EventLog::open_append(paths.events()).map_err(|e| format!("opening the event log: {e}"))?;

    let round = rounds_so_far(&events) + 1;
    println!("revising job {job_id} (round {round})");

    let revision = Revision {
        feedback: &feedback,
        round,
    };
    let outcome = revise_job(
        &config,
        &meta,
        &revision,
        &paths,
        &mut log,
        &machine_opts(&meta.repo),
    )
    .await
    .map_err(|e| e.to_string())?;

    Ok(finish_job(&meta.repo, &config, &meta.base_ref, &paths, outcome).await)
}

/// Report what `gc` would collect, or collect it.
///
/// Policy lives in [`assembly_line::gc`]; this is the printing half.
async fn remove_stale_worktrees(
    older_than: Option<String>,
    dry_run: bool,
) -> Result<ExitCode, String> {
    let keep_for = older_than
        .as_deref()
        .map(config::parse_duration)
        .transpose()
        .map_err(|e| e.to_string())?;

    let found = gc::collectable(keep_for);
    found
        .iter()
        .flat_map(|leftovers| &leftovers.stale)
        .for_each(|entry| {
            println!(
                "{} {} \u{2014} {}",
                match dry_run {
                    true => "would remove",
                    false => "removing",
                },
                entry.path.display(),
                entry.because
            );
        });

    let total = gc::total(&found);
    match (dry_run, total) {
        (_, 0) => println!("nothing to collect"),
        (true, total) => println!("{total} worktree(s) would be removed"),
        (false, _) => {
            let removed = gc::remove(&found).await;
            removed.warnings.iter().for_each(|w| eprintln!("warn: {w}"));
            println!("removed {} worktree(s)", removed.directories);
        }
    }
    Ok(ExitCode::SUCCESS)
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
