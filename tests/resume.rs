use assembly_line::config::parse_graph;
use assembly_line::dag::Dag;
use assembly_line::event::{EventKind, EventLog, RunStatus};
use assembly_line::paths::{create_run, runs_root};
use assembly_line::scheduler::{RunOpts, execute};
use assembly_line::state::{NodeState, RunState};
use assembly_line::workspace;
use assert_cmd::Command;
use predicates::str::contains;
use tokio_util::sync::CancellationToken;

const CHAIN: &str = r#"
[[task]]
id = "one"
kind = "shell"
run = "echo one >> ran.txt"

[[task]]
id = "two"
kind = "shell"
needs = ["one"]
run = "echo two >> ran.txt"

[[task]]
id = "three"
kind = "shell"
needs = ["two"]
run = "echo three >> ran.txt"
"#;

#[tokio::test]
async fn resume_does_not_rerun_completed_nodes() {
    let tmp = tempfile::tempdir().unwrap();
    let graph = parse_graph(CHAIN).unwrap();
    let dag = Dag::build(&graph.tasks).unwrap();
    let paths = create_run(&runs_root(tmp.path()), 1).unwrap();

    // Stand in for a first run that got through "one" and died inside "two".
    {
        let mut log = EventLog::open_append(paths.events()).unwrap();
        [
            EventKind::RunStarted { run_id: 1, jobs: 4 },
            EventKind::NodeStarted {
                node: "one".into(),
                round: 1,
            },
            EventKind::NodeFinished {
                node: "one".into(),
                exit_code: 0,
            },
            EventKind::NodeStarted {
                node: "two".into(),
                round: 1,
            },
        ]
        .into_iter()
        .for_each(|kind| {
            log.append(kind).unwrap();
        });
    }
    std::fs::write(tmp.path().join("ran.txt"), "one\n").unwrap();

    let events = EventLog::read(paths.events()).unwrap();
    let mut state = RunState::replay(dag.ids(), &events);
    assert_eq!(state.state("two"), NodeState::Running);
    assert_eq!(state.reset_running(), vec!["two".to_string()]);

    let mut log = EventLog::open_append(paths.events()).unwrap();
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

    assert_eq!(status, RunStatus::Ok);
    let ran = std::fs::read_to_string(tmp.path().join("ran.txt")).unwrap();
    assert_eq!(
        ran.lines().collect::<Vec<_>>(),
        vec!["one", "two", "three"],
        "'one' must not run twice"
    );
}

#[tokio::test]
async fn resume_appends_to_the_same_log() {
    let tmp = tempfile::tempdir().unwrap();
    let graph = parse_graph(CHAIN).unwrap();
    let dag = Dag::build(&graph.tasks).unwrap();
    let paths = create_run(&runs_root(tmp.path()), 1).unwrap();

    {
        let mut log = EventLog::open_append(paths.events()).unwrap();
        log.append(EventKind::RunStarted { run_id: 1, jobs: 4 })
            .unwrap();
        log.append(EventKind::NodeFinished {
            node: "one".into(),
            exit_code: 0,
        })
        .unwrap();
    }
    let before = EventLog::read(paths.events()).unwrap().len();

    let events = EventLog::read(paths.events()).unwrap();
    let mut state = RunState::replay(dag.ids(), &events);
    let mut log = EventLog::open_append(paths.events()).unwrap();
    let opts = RunOpts {
        jobs: 4,
        cwd: tmp.path().to_path_buf(),
        cancel: CancellationToken::new(),
        repo: None,
        seed_from: tmp.path().to_path_buf(),
        remote: workspace::DEFAULT_REMOTE.to_string(),
    };
    execute(&graph, &dag, &paths, &mut log, &mut state, &opts)
        .await
        .unwrap();

    let after = EventLog::read(paths.events()).unwrap();
    assert!(after.len() > before);
    assert!(matches!(after[0].kind, EventKind::RunStarted { .. }));
    assert_eq!(
        after
            .iter()
            .filter(|e| matches!(e.kind, EventKind::RunStarted { .. }))
            .count(),
        2,
        "each resume records its own run_started"
    );
}

/// The full loop through the binary: interrupt a run, then resume it.
#[test]
fn resuming_an_interrupted_run_through_the_cli_skips_finished_work() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
    std::fs::write(
        tmp.path().join("graph.toml"),
        r#"
[[task]]
id = "one"
kind = "shell"
run = "echo one >> ran.txt"

[[task]]
id = "slow"
kind = "shell"
needs = ["one"]
run = "sleep 30"
max_duration = "20s"
"#,
    )
    .unwrap();

    // A short global cap is not available, so interrupt by timing out the
    // child process itself and treating the killed run as the interruption.
    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("assembly"))
        .current_dir(tmp.path())
        .args(["run", "graph.toml"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();

    // Give "one" time to finish and "slow" time to start, then kill.
    std::thread::sleep(std::time::Duration::from_millis(1500));
    child.kill().unwrap();
    child.wait().unwrap();

    let ran_before = std::fs::read_to_string(tmp.path().join("ran.txt")).unwrap();
    assert_eq!(ran_before.lines().count(), 1, "'one' should have run once");

    // Resume: "one" is already Done, "slow" was Running and gets requeued.
    let mut resumed = std::process::Command::new(assert_cmd::cargo::cargo_bin("assembly"))
        .current_dir(tmp.path())
        .args(["resume", "1"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1500));
    resumed.kill().unwrap();
    resumed.wait().unwrap();

    let ran_after = std::fs::read_to_string(tmp.path().join("ran.txt")).unwrap();
    assert_eq!(
        ran_after.lines().collect::<Vec<_>>(),
        vec!["one"],
        "resume re-executed a completed node"
    );
}

#[test]
fn resume_reports_the_node_it_requeued() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
    std::fs::write(
        tmp.path().join("graph.toml"),
        "[[task]]\nid=\"a\"\nkind=\"shell\"\nrun=\"true\"\n",
    )
    .unwrap();

    let paths = create_run(&runs_root(tmp.path()), 1).unwrap();
    assembly_line::paths::write_meta(
        &paths,
        &assembly_line::paths::RunMeta {
            graph: "graph.toml".into(),
            jobs: 4,
            run_branch: None,
            base_sha: None,
        },
    )
    .unwrap();

    let mut log = EventLog::open_append(paths.events()).unwrap();
    log.append(EventKind::NodeStarted {
        node: "a".into(),
        round: 1,
    })
    .unwrap();
    drop(log);

    Command::cargo_bin("assembly")
        .unwrap()
        .current_dir(tmp.path())
        .args(["resume", "1"])
        .assert()
        .success()
        .stderr(contains("'a' was in flight"));
}
