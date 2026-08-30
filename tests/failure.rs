use assembly_line::config::parse_graph;
use assembly_line::dag::Dag;
use assembly_line::event::{EventKind, EventLog, RunStatus};
use assembly_line::paths::{create_run, runs_root};
use assembly_line::scheduler::{RunOpts, execute};
use assembly_line::state::{NodeState, RunState};
use assembly_line::workspace;
use tokio_util::sync::CancellationToken;

struct Outcome {
    status: RunStatus,
    state: RunState,
    events: Vec<EventKind>,
    _tmp: tempfile::TempDir,
}

async fn run(src: &str) -> Outcome {
    let tmp = tempfile::tempdir().unwrap();
    let graph = parse_graph(src).unwrap();
    let dag = Dag::build(&graph.tasks).unwrap();
    let paths = create_run(&runs_root(tmp.path()), 1).unwrap();
    let mut log = EventLog::open_append(paths.events()).unwrap();
    let mut state = RunState::new(dag.ids());
    let opts = RunOpts {
        jobs: 4,
        cwd: tmp.path().to_path_buf(),
        cancel: CancellationToken::new(),
        repo: None,
        seed_from: tmp.path().to_path_buf(),
        remote: workspace::DEFAULT_REMOTE.to_string(),
    };

    let status = execute(&graph, &dag, &paths, &mut log, &mut state, &opts)
        .await
        .unwrap();
    let events = EventLog::read(paths.events())
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect();

    Outcome {
        status,
        state,
        events,
        _tmp: tmp,
    }
}

#[tokio::test]
async fn a_failure_skips_its_subtree_and_spares_independent_branches() {
    let src = r#"
[[task]]
id = "build"
kind = "shell"
run = "true"

[[task]]
id = "impl-auth"
kind = "shell"
needs = ["build"]
run = "exit 1"

[[task]]
id = "wire-routes"
kind = "shell"
needs = ["impl-auth"]
run = "true"

[[task]]
id = "impl-api"
kind = "shell"
needs = ["build"]
run = "true"
"#;
    let out = run(src).await;

    assert_eq!(out.status, RunStatus::Partial);
    assert_eq!(out.state.state("build"), NodeState::Done);
    assert_eq!(out.state.state("impl-auth"), NodeState::Failed);
    assert_eq!(out.state.state("wire-routes"), NodeState::Skipped);
    assert_eq!(
        out.state.state("impl-api"),
        NodeState::Done,
        "an unrelated branch must still finish"
    );

    assert!(out.events.iter().any(|k| matches!(
        k,
        EventKind::NodeSkipped { node, because }
            if node == "wire-routes" && because.contains("impl-auth")
    )));
}

#[tokio::test]
async fn a_failure_skips_the_whole_subtree_not_just_direct_children() {
    let src = r#"
[[task]]
id = "a"
kind = "shell"
run = "exit 1"

[[task]]
id = "b"
kind = "shell"
needs = ["a"]
run = "true"

[[task]]
id = "c"
kind = "shell"
needs = ["b"]
run = "true"
"#;
    let out = run(src).await;

    assert_eq!(out.state.state("b"), NodeState::Skipped);
    assert_eq!(out.state.state("c"), NodeState::Skipped);
}

#[tokio::test]
async fn on_failure_continue_lets_dependents_run() {
    let src = r#"
[[task]]
id = "lint"
kind = "shell"
run = "exit 1"
on_failure = "continue"

[[task]]
id = "build"
kind = "shell"
needs = ["lint"]
run = "true"
"#;
    let out = run(src).await;

    assert_eq!(out.state.state("lint"), NodeState::Failed);
    assert_eq!(out.state.state("build"), NodeState::Done);
    assert_eq!(
        out.status,
        RunStatus::Partial,
        "a tolerated failure is still a failure"
    );
}

#[tokio::test]
async fn on_failure_abort_stops_the_run_and_skips_the_rest() {
    let src = r#"
[[task]]
id = "migrate"
kind = "shell"
run = "exit 1"
on_failure = "abort"

[[task]]
id = "slow"
kind = "shell"
run = "sleep 5"

[[task]]
id = "later"
kind = "shell"
needs = ["migrate"]
run = "true"
"#;
    let started = std::time::Instant::now();
    let out = run(src).await;

    assert_eq!(out.status, RunStatus::Aborted);
    assert_eq!(out.state.state("later"), NodeState::Skipped);
    assert!(
        started.elapsed().as_secs() < 4,
        "abort should cancel the in-flight sleep, took {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn a_timed_out_node_fails_with_that_reason() {
    let src = r#"
[[task]]
id = "hang"
kind = "shell"
run = "sleep 10"
max_duration = "200ms"
"#;
    let out = run(src).await;

    assert_eq!(out.status, RunStatus::Partial);
    assert_eq!(out.state.state("hang"), NodeState::Failed);
    assert!(
        out.events
            .iter()
            .any(|k| matches!(k, EventKind::NodeFailed { reason, .. } if reason == "timed out"))
    );
}

#[tokio::test]
async fn a_nonzero_exit_records_the_code_in_the_reason() {
    let out = run("[[task]]\nid=\"a\"\nkind=\"shell\"\nrun=\"exit 7\"\n").await;
    assert!(
        out.events
            .iter()
            .any(|k| matches!(k, EventKind::NodeFailed { reason, .. } if reason == "exit 7")),
        "{:?}",
        out.events
    );
}

#[tokio::test]
async fn a_clean_run_is_ok() {
    let out = run("[[task]]\nid=\"a\"\nkind=\"shell\"\nrun=\"true\"\n").await;
    assert_eq!(out.status, RunStatus::Ok);
}
