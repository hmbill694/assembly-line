use assembly_line::cli::{Cli, Command};
use assembly_line::dag::Validation;
use assembly_line::event::{EventKind, EventLog, RunStatus};
use assembly_line::paths::{RunMeta, RunPaths};
use assembly_line::report::RunReport;
use assembly_line::review::{ReviewInbox, ReviewState};
use assembly_line::scheduler::{RunOpts, execute};
use assembly_line::state::RunState;
use assembly_line::{config, dag, delivery, gc, git, paths};
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
        Command::Review {
            run_id,
            approve,
            revise,
        } => review_run(run_id, approve, revise.as_deref()),
        Command::Revise {
            run_id,
            node,
            feedback,
        } => in_async_runtime(revise_node_of_run(run_id, node, feedback)),
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
            deliver_if_complete(&run, &graph, status).await;
            println!("state: {}", run.dir.display());
            exit_code_for(status)
        }
    }
}

/// Hand a finished run's branch on, once the whole graph succeeded.
///
/// A partial run still leaves a real branch, but opening a pull request for
/// work that did not finish is noise — the branch name is printed instead, so
/// acting on it stays a decision rather than a default.
async fn deliver_if_complete(run: &RunPaths, graph: &config::Graph, status: RunStatus) {
    let Some(run_branch) = paths::read_meta(run).ok().and_then(|m| m.run_branch) else {
        return; // A shell-only graph creates no branch to deliver.
    };

    if status != RunStatus::Ok {
        println!("branch: {run_branch} (not delivered — the run did not finish)");
        return;
    }

    let Ok(repo) = enclosing_repo_root() else {
        return;
    };
    // Never an assumed `main`: the base is what the user was standing on. The
    // repository's own HEAD is untouched by a run, so it still says so.
    let base = match graph.delivery.base.clone() {
        Some(declared) => Some(declared),
        None => git::current_branch(&repo).await.ok().flatten(),
    };
    let Some(base) = base else {
        eprintln!("warn: could not tell what branch to deliver onto; branch is {run_branch}");
        return;
    };

    let delivered = delivery::deliver(
        &repo,
        &graph.delivery,
        assembly_line::workspace::DEFAULT_REMOTE,
        &run_branch,
        &base,
    )
    .await;

    match delivered {
        Ok(outcome) => println!("{outcome}"),
        Err(e) => eprintln!("warn: delivering {run_branch}: {e}"),
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

/// Nodes that were built on top of `node`, and so inherit any change to it.
///
/// A verdict passed at 9am on work that merged at 2am is not a rewind: the
/// revision lands *on top* of whatever followed it. Saying how much followed
/// is the difference between an informed approval and a blind one.
fn nodes_built_on(graph_path: &Path, node: &str) -> Vec<String> {
    config::load_graph(graph_path)
        .ok()
        .and_then(|graph| dag::validate(&graph).dag)
        .map(|dag| dag.descendants(node).into_iter().collect())
        .unwrap_or_default()
}

fn report_blast_radius(graph_path: &Path, node: &str) {
    let built_on = nodes_built_on(graph_path, node);
    if !built_on.is_empty() {
        println!(
            "note: {} node(s) branched from this work: {}",
            built_on.len(),
            built_on.join(", ")
        );
    }
}

/// List a run's deferred gates, or record a verdict on one.
///
/// Verdicts are appended to the same log the run wrote. The log is append-only
/// and reviewing happens after the fact, so a verdict is just another event.
fn review_run(run_id: Option<u64>, approve: Option<String>, revise: Option<&[String]>) -> ExitCode {
    let (run, meta) = match locate_run(run_id) {
        Ok(found) => found,
        Err(e) => return fail_with_usage_error(e),
    };

    let events = match EventLog::read(run.events()) {
        Ok(events) => events,
        Err(e) => return fail_with_usage_error(format!("reading the event log: {e}")),
    };
    let inbox = ReviewInbox::from_events(run.id, &events);

    // Clap guarantees the arity and the exclusivity, so the shapes below are
    // the only reachable ones.
    let (node, feedback) = match (approve, revise) {
        (None, None) => {
            print!("{}", inbox.to_terminal_list());
            return ExitCode::SUCCESS;
        }
        (Some(node), _) => (node, None),
        (None, Some([node, feedback])) => (node.clone(), Some(feedback.clone())),
        (None, Some(_)) => return fail_with_usage_error("--revise takes a node and a message"),
    };

    match inbox.item(&node).map(|item| item.state) {
        None => {
            return fail_with_usage_error(format!("run {} has no node '{node}'", run.id));
        }
        Some(ReviewState::Unreviewed) => {}
        Some(settled) => {
            return fail_with_usage_error(format!(
                "'{node}' is {} — only work awaiting review can be ruled on",
                settled.label()
            ));
        }
    }

    let mut log = match EventLog::open_append(run.events()) {
        Ok(log) => log,
        Err(e) => return fail_with_usage_error(format!("opening the event log: {e}")),
    };

    let verdict = match &feedback {
        None => EventKind::NodeApproved { node: node.clone() },
        Some(text) => EventKind::NodeRevisionRequested {
            node: node.clone(),
            feedback: text.clone(),
        },
    };
    if let Err(e) = log.append(verdict) {
        return fail_with_usage_error(format!("recording the verdict: {e}"));
    }

    match feedback {
        None => println!("approved '{node}'"),
        Some(_) => println!(
            "sent '{node}' back — run `assembly revise {} {node}`",
            run.id
        ),
    }
    report_blast_radius(&meta.graph, &node);
    ExitCode::SUCCESS
}

/// How many rounds this node has already had, so the next one is numbered.
fn rounds_so_far(events: &[assembly_line::event::Event], node: &str) -> u32 {
    u32::try_from(
        events
            .iter()
            .filter(|e| matches!(&e.kind, EventKind::NodeStarted { node: n, .. } if n == node))
            .count(),
    )
    .unwrap_or(u32::MAX)
}

/// The feedback `review --revise` recorded for this node, if any.
fn feedback_recorded_for(events: &[assembly_line::event::Event], node: &str) -> Option<String> {
    events.iter().rev().find_map(|e| match &e.kind {
        EventKind::NodeRevisionRequested { node: n, feedback } if n == node => {
            Some(feedback.clone())
        }
        _ => None,
    })
}

/// Run one node again with feedback. The round appends to the node's branch.
async fn revise_node_of_run(run_id: u64, node: String, feedback: Option<String>) -> ExitCode {
    let (run, meta) = match locate_run(Some(run_id)) {
        Ok(found) => found,
        Err(e) => return fail_with_usage_error(e),
    };

    let graph = match config::load_graph(&meta.graph) {
        Ok(graph) => graph,
        Err(e) => return fail_with_usage_error(e),
    };

    let events = match EventLog::read(run.events()) {
        Ok(events) => events,
        Err(e) => return fail_with_usage_error(format!("reading the event log: {e}")),
    };

    // Given on the command line, or left behind by `review --revise`. Without
    // either there is nothing to tell the agent, so this is a usage error
    // rather than a silent no-op round.
    let Some(feedback) = feedback.or_else(|| feedback_recorded_for(&events, &node)) else {
        return fail_with_usage_error(format!(
            "no feedback for '{node}' — pass it here, or record it with \
             `assembly review {run_id} --revise {node} \"...\"`"
        ));
    };

    let repo_root = match enclosing_repo_root() {
        Ok(root) => root,
        Err(e) => return fail_with_usage_error(e),
    };
    let seed_from = match std::env::current_dir() {
        Ok(dir) => dir,
        Err(e) => return fail_with_usage_error(e),
    };

    let ids: Vec<String> = graph.tasks.iter().map(|t| t.id.clone()).collect();
    let mut state = RunState::replay(&ids, &events);
    let mut log = match EventLog::open_append(run.events()) {
        Ok(log) => log,
        Err(e) => return fail_with_usage_error(format!("opening the event log: {e}")),
    };

    let cancel = CancellationToken::new();
    cancel_on_ctrl_c(cancel.clone());

    let opts = RunOpts {
        jobs: 1,
        cwd: repo_root.clone(),
        cancel,
        repo: Some(repo_root),
        seed_from,
        remote: assembly_line::workspace::DEFAULT_REMOTE.to_string(),
    };

    let round = rounds_so_far(&events, &node) + 1;
    println!("revising '{node}' (round {round})");

    let revision = assembly_line::scheduler::Revision {
        node: &node,
        feedback: &feedback,
        round,
    };

    match assembly_line::scheduler::revise_node(
        &graph, &run, &mut log, &mut state, &opts, &revision,
    )
    .await
    {
        Err(e) => fail_with_usage_error(e),
        Ok(node_failed) => {
            print_run_outcome(&run, &meta.graph, RunStatus::Partial, &state);
            match node_failed {
                true => ExitCode::from(EXIT_RUN_INCOMPLETE),
                false => ExitCode::SUCCESS,
            }
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
