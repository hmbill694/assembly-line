use assembly_line::cli::{Cli, Command};
use assembly_line::config::RepoConfig;
use assembly_line::event::{Event, EventKind, EventLog};
use assembly_line::paths::{JobMeta, JobPaths};
use assembly_line::report::JobReport;
use assembly_line::scheduler::{JobSpec, Revision, RunOpts, revise_job, run_job};
use assembly_line::state::JobState;
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

fn in_async_runtime(work: impl Future<Output = ExitCode>) -> ExitCode {
    match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime.block_on(work),
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

/// The prompt the user supplied, inline or by file.
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

/// Everything the repository declares plus the provider the job will use,
/// with every problem reported at once.
fn config_and_provider(
    config: RepoConfig,
    chosen: Option<String>,
) -> Result<(RepoConfig, String), String> {
    let provider = chosen
        .or_else(|| config.provider.clone())
        .unwrap_or_default();

    config
        .warnings()
        .iter()
        .for_each(|w| eprintln!("warn: {w}"));

    let problems = config.problems(&provider);
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
) -> ExitCode {
    let ready = match prepare_job(prompt, prompt_file, repo, base_ref, provider).await {
        Ok(ready) => ready,
        Err(e) => return fail_with_usage_error(e),
    };
    let PreparedJob {
        repo,
        base_ref,
        prompt,
        provider,
        config,
    } = ready;

    // Allocated only once the config is known good, so a repository that has
    // not opted in leaves no litter.
    let jobs_root = paths::jobs_root(&repo);
    let paths =
        match paths::next_job_id(&jobs_root).and_then(|id| paths::create_job(&jobs_root, id)) {
            Ok(p) => p,
            Err(e) => return fail_with_usage_error(format!("preparing the job directory: {e}")),
        };

    let meta = JobMeta {
        repo: repo.clone(),
        base_ref: base_ref.clone(),
        prompt: prompt.clone(),
        provider: provider.clone(),
        branch: None,
    };
    if let Err(e) = paths::write_meta(&paths, &meta) {
        return fail_with_usage_error(format!("writing meta.json: {e}"));
    }

    let mut log = match EventLog::open_append(paths.events()) {
        Ok(log) => log,
        Err(e) => return fail_with_usage_error(format!("opening the event log: {e}")),
    };
    let mut state = JobState::default();
    let spec = JobSpec {
        prompt: &prompt,
        provider: &provider,
        base_ref: &base_ref,
        round: 1,
    };

    let outcome = run_job(
        &config,
        &spec,
        &paths,
        &mut log,
        &mut state,
        &machine_opts(&repo),
    )
    .await;

    match outcome {
        Err(e) => fail_with_usage_error(e),
        Ok(job_failed) => {
            let meta = report_and_record_branch(&paths, meta);
            deliver_if_verified(&repo, &config, &meta, job_failed).await;
            println!("state: {}", paths.dir.display());
            exit_code_for(job_failed)
        }
    }
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
    let (config, provider) = config_and_provider(declared, provider)?;

    Ok(PreparedJob {
        repo,
        base_ref,
        prompt,
        provider,
        config,
    })
}

fn exit_code_for(job_failed: bool) -> ExitCode {
    match job_failed {
        true => ExitCode::from(EXIT_JOB_FAILED),
        false => ExitCode::SUCCESS,
    }
}

fn events_of(paths: &JobPaths) -> Result<Vec<Event>, String> {
    EventLog::read(paths.events()).map_err(|e| format!("reading the event log: {e}"))
}

/// Print what the job's own log says became of it, and record the branch it
/// left in `meta.json` so `revise` and delivery can find it by id alone.
///
/// Returns the meta as recorded, branch included, so the caller can hand it
/// straight to delivery without re-reading what was just written.
fn report_and_record_branch(paths: &JobPaths, meta: JobMeta) -> JobMeta {
    let Ok(events) = events_of(paths) else {
        return meta;
    };
    let report = JobReport::from_events(paths.id, &events);
    println!("{}", report.to_summary_line());

    match report.branch {
        Some(branch) => {
            let meta = JobMeta {
                branch: Some(branch),
                ..meta
            };
            let _ = paths::write_meta(paths, &meta);
            meta
        }
        None => meta,
    }
}

/// Hand a finished job's branch on, once `verify` accepted it.
///
/// A failed job still leaves a real branch, but opening a pull request for
/// work that did not pass is noise — the branch name is printed instead, so
/// acting on it stays a decision rather than a default.
async fn deliver_if_verified(repo: &Path, config: &RepoConfig, meta: &JobMeta, failed: bool) {
    let Some(branch) = &meta.branch else {
        return; // The agent changed nothing, so there is nothing to deliver.
    };

    if failed {
        println!("branch: {branch} (not delivered — the job did not pass)");
        return;
    }

    let base = config.base.clone().unwrap_or_else(|| meta.base_ref.clone());

    if let Some(configured) = config.base.as_deref().filter(|&b| b != meta.base_ref) {
        println!(
            "note: this pull request will target '{configured}', but the job was cut from '{}' — review the diff before merging, since it carries everything separating the two, not just this job's work",
            meta.base_ref
        );
    }

    match delivery::deliver(
        repo,
        &config.delivery,
        assembly_line::workspace::DEFAULT_REMOTE,
        branch,
        &base,
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
async fn revise_existing_job(job_id: u64, feedback: String, repo: Option<PathBuf>) -> ExitCode {
    let (paths, meta) = match locate_job(Some(job_id), repo) {
        Ok(found) => found,
        Err(e) => return fail_with_usage_error(e),
    };
    let events = match events_of(&paths) {
        Ok(events) => events,
        Err(e) => return fail_with_usage_error(e),
    };

    // The ref the job was cut from, not the job's own branch: the settings
    // that govern a revise are the ones the repository declared, which the
    // previous round's branch is not allowed to have changed.
    let declared = match RepoConfig::from_ref(&meta.repo, &meta.base_ref).await {
        Ok(config) => config,
        Err(e) => return fail_with_usage_error(e),
    };
    let (config, _) = match config_and_provider(declared, Some(meta.provider.clone())) {
        Ok(ready) => ready,
        Err(e) => return fail_with_usage_error(e),
    };

    let mut log = match EventLog::open_append(paths.events()) {
        Ok(log) => log,
        Err(e) => return fail_with_usage_error(format!("opening the event log: {e}")),
    };
    let mut state = JobState::replay(&events);

    let round = rounds_so_far(&events) + 1;
    println!("revising job {job_id} (round {round})");

    let opts = machine_opts(&meta.repo);
    let revision = Revision {
        feedback: &feedback,
        round,
    };
    match revise_job(
        &config, &meta, &revision, &paths, &mut log, &mut state, &opts,
    )
    .await
    {
        Err(e) => fail_with_usage_error(e),
        Ok(job_failed) => {
            let meta = report_and_record_branch(&paths, meta);
            deliver_if_verified(&meta.repo, &config, &meta, job_failed).await;
            exit_code_for(job_failed)
        }
    }
}

/// Report what `gc` would collect, or collect it.
///
/// Policy lives in [`assembly_line::gc`]; this is the printing half.
async fn remove_stale_worktrees(older_than: Option<String>, dry_run: bool) -> ExitCode {
    let keep_for = match older_than
        .as_deref()
        .map(config::parse_duration)
        .transpose()
    {
        Ok(d) => d,
        Err(e) => return fail_with_usage_error(e),
    };

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
    ExitCode::SUCCESS
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
        // `tail -f` is the right tool and is present everywhere this runs.
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
