use assembly_line::cli::{Cli, Command};
use assembly_line::dag::Validation;
use assembly_line::event::{EventKind, EventLog, RunStatus};
use assembly_line::paths::{RunMeta, RunPaths};
use assembly_line::report::RunReport;
use assembly_line::scheduler::{RunOpts, execute};
use assembly_line::state::RunState;
use assembly_line::{config, dag, git, paths};
use clap::Parser;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use tokio_util::sync::CancellationToken;

/// Reserved for the user's mistake — a bad graph, a missing run, a wrong
/// directory. A run that executes and fails exits 1 instead.
const EXIT_USAGE: u8 = 2;
const EXIT_RUN_INCOMPLETE: u8 = 1;

fn main() -> ExitCode {
    install_tracing();

    match Cli::parse().command {
        Command::Validate { graph } => validate_graph_file(&graph),
        Command::Run { graph, jobs } => in_async_runtime(start_new_run(graph, jobs)),
        Command::Resume { run_id, jobs } => in_async_runtime(continue_existing_run(run_id, jobs)),
        Command::Status { run_id } => print_run_status(run_id),
        Command::Gc {
            older_than,
            dry_run,
        } => in_async_runtime(remove_stale_worktrees(older_than, dry_run)),
        Command::Logs {
            run_id,
            node,
            follow,
        } => print_node_log(run_id, &node, follow),
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

fn print_warnings(validation: &Validation) {
    validation
        .warnings
        .iter()
        .for_each(|w| eprintln!("warn: {w}"));
}

fn print_errors(validation: &Validation) {
    validation
        .errors
        .iter()
        .for_each(|e| eprintln!("error: {e}"));
}

fn validate_graph_file(path: &Path) -> ExitCode {
    let graph = match config::load_graph(path) {
        Ok(g) => g,
        Err(e) => return fail_with_usage_error(e),
    };

    let validation = dag::validate(&graph);
    print_warnings(&validation);

    if validation.errors.is_empty() {
        println!("{}: ok ({} tasks)", path.display(), graph.tasks.len());
        ExitCode::SUCCESS
    } else {
        print_errors(&validation);
        ExitCode::from(EXIT_USAGE)
    }
}

/// Run state lives under the repo, so every command needs to know which repo.
fn enclosing_repo_root() -> Result<PathBuf, String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    paths::git_root(&cwd).ok_or_else(|| {
        "not inside a git repository — assembly stores run state at <git-root>/.assembly".into()
    })
}

fn cancel_on_ctrl_c(cancel: CancellationToken) {
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("\ninterrupted — cancelling in-flight nodes");
            cancel.cancel();
        }
    });
}

fn exit_code_for(status: RunStatus) -> ExitCode {
    match status {
        RunStatus::Ok => ExitCode::SUCCESS,
        RunStatus::Partial | RunStatus::Aborted => ExitCode::from(EXIT_RUN_INCOMPLETE),
    }
}

async fn drive_run(
    graph_path: &Path,
    run: &RunPaths,
    jobs: usize,
    state: RunState,
) -> Result<(RunStatus, RunState), String> {
    let graph = config::load_graph(graph_path).map_err(|e| e.to_string())?;
    let validation = dag::validate(&graph);
    print_warnings(&validation);

    let Some(dag) = validation.dag else {
        print_errors(&validation);
        return Err(format!("{} is not runnable", graph_path.display()));
    };

    let repo_root = enclosing_repo_root()?;
    let mut log = EventLog::open_append(run.events()).map_err(|e| e.to_string())?;
    let mut state = state;

    let cancel = CancellationToken::new();
    cancel_on_ctrl_c(cancel.clone());

    // `copy` paths are written relative to where the user invoked assembly,
    // so a graph plus its uncommitted files stays a movable unit.
    let seed_from = std::env::current_dir().map_err(|e| e.to_string())?;
    let opts = RunOpts {
        jobs,
        cwd: repo_root.clone(),
        cancel,
        repo: Some(repo_root),
        seed_from,
        remote: assembly_line::workspace::DEFAULT_REMOTE.to_string(),
    };
    let status = execute(&graph, &dag, run, &mut log, &mut state, &opts)
        .await
        .map_err(|e| e.to_string())?;

    Ok((status, state))
}

/// Copy the run branch into `meta.json`, so `status` and future tooling can
/// find the work without replaying the log.
///
/// Read back from the event log rather than threaded out of the scheduler: the
/// log is the record of what happened, and a graph without agent nodes creates
/// no branch to record. A failure here is reported but does not fail the run —
/// the branch exists either way.
fn record_run_branch_in_meta(run: &RunPaths, meta: &RunMeta) {
    let Some((run_branch, base_sha)) = EventLog::read(run.events())
        .ok()
        .and_then(|events| events.into_iter().find_map(branch_creation))
    else {
        return;
    };

    let recorded = RunMeta {
        run_branch: Some(run_branch),
        base_sha: Some(base_sha),
        ..meta.clone()
    };
    if let Err(e) = paths::write_meta(run, &recorded) {
        eprintln!("warn: could not record the run branch in meta.json: {e}");
    }
}

fn branch_creation(event: assembly_line::event::Event) -> Option<(String, String)> {
    match event.kind {
        EventKind::RunBranchCreated { branch, base_sha } => Some((branch, base_sha)),
        _ => None,
    }
}

fn report_for(run: &RunPaths, graph_path: &Path) -> Result<RunReport, String> {
    let graph = config::load_graph(graph_path).map_err(|e| e.to_string())?;
    let ids: Vec<String> = graph.tasks.iter().map(|t| t.id.clone()).collect();
    let events = EventLog::read(run.events()).map_err(|e| e.to_string())?;
    Ok(RunReport::from_events(run.id, &ids, &events))
}

async fn start_new_run(graph_path: PathBuf, jobs: usize) -> ExitCode {
    // Validate before allocating a run directory, so a typo leaves no litter.
    let graph = match config::load_graph(&graph_path) {
        Ok(g) => g,
        Err(e) => return fail_with_usage_error(e),
    };
    let validation = dag::validate(&graph);
    if !validation.errors.is_empty() {
        print_warnings(&validation);
        print_errors(&validation);
        return ExitCode::from(EXIT_USAGE);
    }

    let repo_root = match enclosing_repo_root() {
        Ok(r) => r,
        Err(e) => return fail_with_usage_error(e),
    };

    let runs_root = paths::runs_root(&repo_root);
    let run = match paths::next_run_id(&runs_root).and_then(|id| paths::create_run(&runs_root, id))
    {
        Ok(r) => r,
        Err(e) => return fail_with_usage_error(format!("preparing the run directory: {e}")),
    };

    let meta = RunMeta {
        graph: graph_path.clone(),
        jobs,
        run_branch: None,
        base_sha: None,
    };
    if let Err(e) = paths::write_meta(&run, &meta) {
        return fail_with_usage_error(format!("writing meta.json: {e}"));
    }

    let initial = RunState::new(&graph.tasks.iter().map(|t| t.id.clone()).collect::<Vec<_>>());
    match drive_run(&graph_path, &run, jobs, initial).await {
        Err(e) => fail_with_usage_error(e),
        Ok((status, state)) => {
            record_run_branch_in_meta(&run, &meta);
            print_run_outcome(&run, &graph_path, status, &state);
            println!("state: {}", run.dir.display());
            exit_code_for(status)
        }
    }
}

async fn continue_existing_run(run_id: u64, jobs_override: Option<usize>) -> ExitCode {
    let (run, meta) = match locate_run(Some(run_id)) {
        Ok(found) => found,
        Err(e) => return fail_with_usage_error(e),
    };

    let graph = match config::load_graph(&meta.graph) {
        Ok(g) => g,
        Err(e) => return fail_with_usage_error(e),
    };
    let ids: Vec<String> = graph.tasks.iter().map(|t| t.id.clone()).collect();

    let events = match EventLog::read(run.events()) {
        Ok(e) => e,
        Err(e) => return fail_with_usage_error(format!("reading the event log: {e}")),
    };

    let mut state = RunState::replay(&ids, &events);
    state.reset_running().iter().for_each(|node| {
        eprintln!("note: '{node}' was in flight when the run stopped — running it again");
    });

    let jobs = jobs_override.unwrap_or(meta.jobs);
    match drive_run(&meta.graph, &run, jobs, state).await {
        Err(e) => fail_with_usage_error(e),
        Ok((status, state)) => {
            print_run_outcome(&run, &meta.graph, status, &state);
            exit_code_for(status)
        }
    }
}

/// Prefer the event log's own account; fall back to in-memory counts if the
/// log cannot be re-read for any reason.
fn print_run_outcome(run: &RunPaths, graph_path: &Path, status: RunStatus, state: &RunState) {
    if let Ok(report) = report_for(run, graph_path) {
        println!("{}", report.to_summary_line());
    } else {
        let counts = state.counts();
        println!(
            "run {}: {} — {} done, {} failed, {} skipped",
            run.id,
            status.label(),
            counts.done,
            counts.failed,
            counts.skipped
        );
    }
}

fn locate_run(run_id: Option<u64>) -> Result<(RunPaths, RunMeta), String> {
    let repo_root = enclosing_repo_root()?;
    let runs_root = paths::runs_root(&repo_root);

    let id = match run_id {
        Some(id) => id,
        None => paths::latest_run_id(&runs_root)
            .map_err(|e| e.to_string())?
            .ok_or("no runs yet")?,
    };

    let run = paths::open_run(&runs_root, id).map_err(|e| e.to_string())?;
    let meta =
        paths::read_meta(&run).map_err(|e| format!("reading meta.json for run {id}: {e}"))?;
    Ok((run, meta))
}

fn print_run_status(run_id: Option<u64>) -> ExitCode {
    let outcome = locate_run(run_id)
        .and_then(|(run, meta)| report_for(&run, &meta.graph).map(|r| r.to_terminal_tree()));

    match outcome {
        Ok(tree) => {
            print!("{tree}");
            ExitCode::SUCCESS
        }
        Err(e) => fail_with_usage_error(e),
    }
}

/// A run's worktree directory that nothing needs any more.
struct StaleWorktree {
    path: PathBuf,
    because: String,
}

/// Directories directly under `dir` whose names are run ids.
fn run_directories(dir: &Path) -> Vec<(u64, PathBuf)> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let id = entry.file_name().to_str()?.parse::<u64>().ok()?;
            Some((id, entry.path()))
        })
        .collect()
}

fn subdirectories(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect()
}

/// How long ago `path` was last written, or `None` if that cannot be read.
fn idle_time(path: &Path) -> Option<std::time::Duration> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|written| written.elapsed().ok())
}

/// Why a run's worktree directory is collectable, or `None` while it is still
/// the record of a run that exists.
///
/// Successful nodes discard their own worktrees; what `gc` finds is what
/// failures deliberately kept, plus anything a crash orphaned.
fn reason_to_collect(
    repo: Option<&Path>,
    run_id: u64,
    path: &Path,
    keep_for: Option<std::time::Duration>,
) -> Option<String> {
    // Age only collects when the user asked for it; without `--older-than`, a
    // worktree whose run still exists is still wanted however old it is.
    let too_old = || {
        let keep_for = keep_for?;
        let idle = idle_time(path)?;
        (idle > keep_for).then(|| {
            format!(
                "untouched for {}",
                humantime::format_duration(std::time::Duration::from_secs(idle.as_secs()))
            )
        })
    };

    match repo {
        None => Some("its repository is unknown".to_string()),
        Some(repo) if !repo.exists() => Some(format!("{} no longer exists", repo.display())),
        Some(repo) if !paths::runs_root(repo).join(run_id.to_string()).is_dir() => {
            Some(format!("run {run_id} has no state directory"))
        }
        Some(_) => too_old(),
    }
}

/// Every collectable worktree directory, paired with the repository it belongs
/// to so the caller can prune that repository's worktree list afterwards.
fn collectable_worktrees(
    keep_for: Option<std::time::Duration>,
) -> Vec<(Option<PathBuf>, Vec<StaleWorktree>)> {
    let Some(root) = paths::worktrees_root() else {
        return Vec::new();
    };

    subdirectories(&root)
        .into_iter()
        .map(|per_repo| {
            let repo = paths::repository_owning_worktrees(&per_repo);
            let stale = run_directories(&per_repo)
                .into_iter()
                .filter_map(|(run_id, path)| {
                    reason_to_collect(repo.as_deref(), run_id, &path, keep_for)
                        .map(|because| StaleWorktree { path, because })
                })
                .collect();
            (repo, stale)
        })
        .collect()
}

async fn remove_stale_worktrees(older_than: Option<String>, dry_run: bool) -> ExitCode {
    let keep_for = match older_than
        .as_deref()
        .map(config::parse_duration)
        .transpose()
    {
        Ok(d) => d,
        Err(e) => return fail_with_usage_error(e),
    };

    let found = collectable_worktrees(keep_for);
    let mut removed = 0usize;

    for (repo, stale) in &found {
        for entry in stale {
            println!(
                "{} {} — {}",
                match dry_run {
                    true => "would remove",
                    false => "removing",
                },
                entry.path.display(),
                entry.because
            );
            if !dry_run {
                if let Err(e) = std::fs::remove_dir_all(&entry.path) {
                    eprintln!("warn: could not remove {}: {e}", entry.path.display());
                    continue;
                }
                removed += 1;
            }
        }

        // Git still lists a worktree whose directory is gone, and will refuse
        // to reuse the path until it is told otherwise.
        if let Some(repo) = repo
            && !dry_run
            && !stale.is_empty()
            && repo.exists()
            && let Err(e) = git::prune_worktrees(repo).await
        {
            eprintln!("warn: could not prune worktrees in {}: {e}", repo.display());
        }
    }

    let total: usize = found.iter().map(|(_, stale)| stale.len()).sum();
    match (dry_run, total) {
        (_, 0) => println!("nothing to collect"),
        (true, total) => println!("{total} worktree(s) would be removed"),
        (false, _) => println!("removed {removed} worktree(s)"),
    }
    ExitCode::SUCCESS
}

fn print_node_log(run_id: u64, node: &str, follow: bool) -> ExitCode {
    let run = match locate_run(Some(run_id)) {
        Ok((run, _)) => run,
        Err(e) => return fail_with_usage_error(e),
    };

    let path = run.log(node);
    if !path.exists() {
        return fail_with_usage_error(format!("no log for node '{node}' in run {run_id}"));
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
