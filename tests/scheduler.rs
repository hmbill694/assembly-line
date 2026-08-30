use assembly_line::config::parse_graph;
use assembly_line::dag::Dag;
use assembly_line::event::{EventKind, EventLog, RunStatus};
use assembly_line::paths::{create_run, runs_root};
use assembly_line::scheduler::{RunOpts, execute};
use assembly_line::state::{NodeState, RunState};
use assembly_line::workspace;
use tokio_util::sync::CancellationToken;

struct Harness {
    tmp: tempfile::TempDir,
}

struct Outcome {
    status: RunStatus,
    events: Vec<EventKind>,
    state: RunState,
}

impl Harness {
    fn new() -> Self {
        Harness {
            tmp: tempfile::tempdir().unwrap(),
        }
    }

    async fn run(&self, src: &str, jobs: usize) -> Outcome {
        let graph = parse_graph(src).unwrap();
        let dag = Dag::build(&graph.tasks).unwrap();
        let paths = create_run(&runs_root(self.tmp.path()), 1).unwrap();
        let mut log = EventLog::open_append(paths.events()).unwrap();
        let mut state = RunState::new(dag.ids());
        let opts = RunOpts {
            jobs,
            cwd: self.tmp.path().to_path_buf(),
            cancel: CancellationToken::new(),
            repo: None,
            seed_from: self.tmp.path().to_path_buf(),
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
            events,
            state,
        }
    }

    fn read(&self, file: &str) -> String {
        std::fs::read_to_string(self.tmp.path().join(file)).unwrap()
    }

    /// Highest concurrency the probe observed.
    fn peak(&self, file: &str) -> usize {
        self.read(file)
            .lines()
            .filter_map(|l| l.trim().parse::<usize>().ok())
            .max()
            .expect("probe recorded nothing")
    }
}

/// A shell body that records how many copies of itself were live on entry.
fn probe(dir: &str, out: &str) -> String {
    format!("mkdir -p {dir}; touch {dir}/$$; ls {dir} | wc -l >> {out}; sleep 0.3; rm {dir}/$$")
}

fn shell_tasks(bodies: &[(&str, &str)]) -> String {
    bodies
        .iter()
        .map(|(id, body)| format!("[[task]]\nid = \"{id}\"\nkind = \"shell\"\nrun = \"{body}\"\n"))
        .collect()
}

#[tokio::test]
async fn runs_a_linear_chain_in_order() {
    let h = Harness::new();
    let src = r#"
[[task]]
id = "one"
kind = "shell"
run = "echo one >> order.txt"

[[task]]
id = "two"
kind = "shell"
needs = ["one"]
run = "echo two >> order.txt"

[[task]]
id = "three"
kind = "shell"
needs = ["two"]
run = "echo three >> order.txt"
"#;
    let out = h.run(src, 4).await;

    assert_eq!(out.status, RunStatus::Ok);
    assert_eq!(
        h.read("order.txt").lines().collect::<Vec<_>>(),
        vec!["one", "two", "three"]
    );
}

#[tokio::test]
async fn independent_nodes_actually_overlap() {
    let h = Harness::new();
    // Three 400ms sleeps: ~1.2s serially, ~0.4s in parallel.
    let src = shell_tasks(&[("a", "sleep 0.4"), ("b", "sleep 0.4"), ("c", "sleep 0.4")]);

    let start = std::time::Instant::now();
    let out = h.run(&src, 4).await;
    let elapsed = start.elapsed();

    assert_eq!(out.status, RunStatus::Ok);
    assert!(
        elapsed.as_millis() < 900,
        "took {elapsed:?}, expected overlap"
    );
}

#[tokio::test]
async fn jobs_one_serializes() {
    let h = Harness::new();
    let body = probe("conc", "seen.txt");
    let src = shell_tasks(&[("a", &body), ("b", &body), ("c", &body)]);

    assert_eq!(h.run(&src, 1).await.status, RunStatus::Ok);
    assert_eq!(h.peak("seen.txt"), 1);
}

#[tokio::test]
async fn jobs_cap_is_respected() {
    let h = Harness::new();
    let body = probe("conc", "seen.txt");
    let src = shell_tasks(&[("a", &body), ("b", &body), ("c", &body), ("d", &body)]);

    assert_eq!(h.run(&src, 2).await.status, RunStatus::Ok);
    assert!(h.peak("seen.txt") <= 2, "jobs cap not honored");
}

#[tokio::test]
async fn nodes_sharing_a_resource_never_overlap() {
    let h = Harness::new();
    let body = probe("db", "db_seen.txt");
    let src: String = ["a", "b", "c"]
        .iter()
        .map(|id| {
            format!(
                "[[task]]\nid = \"{id}\"\nkind = \"shell\"\nresource = \"postgres\"\nrun = \"{body}\"\n"
            )
        })
        .collect();

    assert_eq!(h.run(&src, 4).await.status, RunStatus::Ok);
    assert_eq!(
        h.peak("db_seen.txt"),
        1,
        "resource-exclusive nodes overlapped"
    );
}

#[tokio::test]
async fn different_resources_still_overlap() {
    let h = Harness::new();
    let src = format!(
        "[[task]]\nid = \"a\"\nkind = \"shell\"\nresource = \"pg\"\nrun = \"{}\"\n\
         [[task]]\nid = \"b\"\nkind = \"shell\"\nresource = \"redis\"\nrun = \"{}\"\n",
        probe("both", "seen.txt"),
        probe("both", "seen.txt")
    );

    assert_eq!(h.run(&src, 4).await.status, RunStatus::Ok);
    assert_eq!(h.peak("seen.txt"), 2, "distinct resources should not block");
}

#[tokio::test]
async fn writes_a_log_file_per_node() {
    let h = Harness::new();
    let graph = parse_graph("[[task]]\nid=\"a\"\nkind=\"shell\"\nrun=\"echo marker\"\n").unwrap();
    let dag = Dag::build(&graph.tasks).unwrap();
    let paths = create_run(&runs_root(h.tmp.path()), 9).unwrap();
    let mut log = EventLog::open_append(paths.events()).unwrap();
    let mut state = RunState::new(dag.ids());
    let opts = RunOpts {
        jobs: 1,
        cwd: h.tmp.path().to_path_buf(),
        cancel: CancellationToken::new(),
        repo: None,
        seed_from: h.tmp.path().to_path_buf(),
        remote: workspace::DEFAULT_REMOTE.to_string(),
    };

    execute(&graph, &dag, &paths, &mut log, &mut state, &opts)
        .await
        .unwrap();

    let body = std::fs::read_to_string(paths.log("a")).unwrap();
    assert!(body.contains("marker"), "{body}");
}

#[tokio::test]
async fn emits_started_and_finished_events_for_every_node() {
    let h = Harness::new();
    let out = h
        .run("[[task]]\nid=\"a\"\nkind=\"shell\"\nrun=\"true\"\n", 1)
        .await;

    assert!(matches!(out.events[0], EventKind::RunStarted { .. }));
    assert!(
        out.events
            .iter()
            .any(|k| matches!(k, EventKind::NodeStarted { node, round: 1 } if node == "a"))
    );
    assert!(
        out.events
            .iter()
            .any(|k| matches!(k, EventKind::NodeFinished { node, exit_code: 0 } if node == "a"))
    );
    assert!(matches!(
        out.events.last().unwrap(),
        EventKind::RunFinished {
            status: RunStatus::Ok
        }
    ));
}

/// Agent execution itself is covered in `tests/agent_nodes.rs`, against a real
/// repository. This harness deliberately has none, which is the case that must
/// fail rather than half-run.
#[tokio::test]
async fn an_agent_node_without_a_repository_fails_saying_so() {
    let h = Harness::new();
    let src = "[providers.p]\ncmd=\"true\"\n\
               [[task]]\nid=\"a\"\nkind=\"agent\"\nprompt=\"hi\"\nprovider=\"p\"\nverify=\"true\"\n";
    let out = h.run(src, 1).await;

    assert_eq!(out.status, RunStatus::Partial);
    assert_eq!(out.state.state("a"), NodeState::Failed);
    assert!(
        out.events.iter().any(
            |k| matches!(k, EventKind::NodeFailed { reason, .. } if reason.contains("repository"))
        ),
        "{:?}",
        out.events
    );
}

#[tokio::test]
async fn an_empty_graph_is_ok() {
    let h = Harness::new();
    let out = h.run("", 4).await;
    assert_eq!(out.status, RunStatus::Ok);
}
