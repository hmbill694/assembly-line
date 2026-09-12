use assembly_line::config::parse_graph;
use assembly_line::dag::Dag;
use assembly_line::event::{EventKind, EventLog, RunStatus};
use assembly_line::git::{self, commit_all};
use assembly_line::paths::{create_run, repo_worktrees_root, runs_root};
use assembly_line::scheduler::{RunOpts, execute};
use assembly_line::state::{NodeState, RunState};
use assembly_line::workspace::{self, run_branch_name};
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

/// A repository with one commit, so agent nodes have somewhere to branch
/// from. Every task is an agent task now, so every scheduling test needs one.
struct Harness {
    tmp: tempfile::TempDir,
    repo: PathBuf,
}

/// Worktrees live under `$HOME`. A run that dies mid-node can orphan one, and
/// tests must not leave that behind.
impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(root) = repo_worktrees_root(&self.repo) {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

struct Outcome {
    status: RunStatus,
    events: Vec<EventKind>,
    run_id: u64,
}

impl Harness {
    async fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [
            vec!["init", "--initial-branch=main"],
            vec!["config", "user.email", "t@e.com"],
            vec!["config", "user.name", "T"],
            vec!["config", "commit.gpgsign", "false"],
        ] {
            git::run_allowing_failure(&repo, &args).await.unwrap();
        }
        std::fs::write(repo.join("README.md"), "base\n").unwrap();
        commit_all(&repo, "initial").await.unwrap().unwrap();

        Harness { tmp, repo }
    }

    /// A script that touches nothing in the node's own worktree, so it can be
    /// run concurrently without merge conflicts. `dir` and `out` are absolute
    /// paths outside the repo, so every node's process — each in its own
    /// worktree — still observes the same files.
    fn probe(&self, dir: &str, out: &str) -> String {
        let dir = self.tmp.path().join(dir);
        let out = self.tmp.path().join(out);
        format!(
            "mkdir -p {d}; touch {d}/$$; ls {d} | wc -l >> {o}; sleep 0.3; rm {d}/$$",
            d = dir.display(),
            o = out.display()
        )
    }

    async fn run(&self, src: &str, jobs: usize) -> Outcome {
        let graph = parse_graph(src).unwrap();
        let dag = Dag::build(&graph.tasks).unwrap();
        let paths = create_run(&runs_root(self.tmp.path()), 1).unwrap();
        let mut log = EventLog::open_append(paths.events()).unwrap();
        let mut state = RunState::new(dag.ids());
        let opts = RunOpts {
            jobs,
            cwd: self.repo.clone(),
            cancel: CancellationToken::new(),
            repo: Some(self.repo.clone()),
            seed_from: self.repo.clone(),
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
            run_id: paths.id,
        }
    }

    /// Highest concurrency the probe observed.
    fn peak(&self, file: &str) -> usize {
        std::fs::read_to_string(self.tmp.path().join(file))
            .unwrap()
            .lines()
            .filter_map(|l| l.trim().parse::<usize>().ok())
            .max()
            .expect("probe recorded nothing")
    }
}

/// One `[providers.run]` block whose command is the task's own prompt, run
/// through a shell. Lets a test hand each node an arbitrary script without
/// needing a fixture file for it.
const RUN_PROVIDER: &str = "[providers.run]\ncmd = \"bash\"\nargs = [\"-c\", \"{prompt}\"]\n";

fn agent_tasks(bodies: &[(&str, &str)]) -> String {
    let tasks: String = bodies
        .iter()
        .map(|(id, body)| {
            format!("[[task]]\nid = \"{id}\"\nprovider = \"run\"\nprompt = \"{body}\"\n")
        })
        .collect();
    format!("{RUN_PROVIDER}\n{tasks}")
}

fn agent_tasks_with_resource(bodies: &[(&str, &str, &str)]) -> String {
    let tasks: String = bodies
        .iter()
        .map(|(id, resource, body)| {
            format!(
                "[[task]]\nid = \"{id}\"\nprovider = \"run\"\nresource = \"{resource}\"\nprompt = \"{body}\"\n"
            )
        })
        .collect();
    format!("{RUN_PROVIDER}\n{tasks}")
}

/// Dependency order is what the scheduler exists to enforce, so it must still
/// hold once every node is an agent: each node's workspace starts from
/// whatever the run branch carries when it launches, which is only ever what
/// finished before it.
#[tokio::test]
async fn runs_a_linear_chain_in_order() {
    let h = Harness::new().await;
    let src = format!(
        "{RUN_PROVIDER}\n\
         [[task]]\nid = \"one\"\nprovider = \"run\"\nprompt = \"printf 'one\\\\n' >> order.txt\"\n\
         [[task]]\nid = \"two\"\nneeds = [\"one\"]\nprovider = \"run\"\nprompt = \"printf 'two\\\\n' >> order.txt\"\n\
         [[task]]\nid = \"three\"\nneeds = [\"two\"]\nprovider = \"run\"\nprompt = \"printf 'three\\\\n' >> order.txt\"\n"
    );
    let out = h.run(&src, 4).await;

    assert_eq!(out.status, RunStatus::Ok);
    let branch = run_branch_name(out.run_id);
    let content = git::run_allowing_failure(&h.repo, &["show", &format!("{branch}:order.txt")])
        .await
        .unwrap()
        .stdout;
    assert_eq!(
        content.lines().collect::<Vec<_>>(),
        vec!["one", "two", "three"]
    );
}

#[tokio::test]
async fn jobs_one_serializes() {
    let h = Harness::new().await;
    let body = h.probe("conc", "seen.txt");
    let src = agent_tasks(&[("a", &body), ("b", &body), ("c", &body)]);

    assert_eq!(h.run(&src, 1).await.status, RunStatus::Ok);
    assert_eq!(h.peak("seen.txt"), 1);
}

#[tokio::test]
async fn jobs_cap_is_respected() {
    let h = Harness::new().await;
    let body = h.probe("conc", "seen.txt");
    let src = agent_tasks(&[("a", &body), ("b", &body), ("c", &body), ("d", &body)]);

    assert_eq!(h.run(&src, 2).await.status, RunStatus::Ok);
    assert!(h.peak("seen.txt") <= 2, "jobs cap not honored");
}

#[tokio::test]
async fn nodes_sharing_a_resource_never_overlap() {
    let h = Harness::new().await;
    let body = h.probe("db", "db_seen.txt");
    let src = agent_tasks_with_resource(&[
        ("a", "postgres", &body),
        ("b", "postgres", &body),
        ("c", "postgres", &body),
    ]);

    assert_eq!(h.run(&src, 4).await.status, RunStatus::Ok);
    assert_eq!(
        h.peak("db_seen.txt"),
        1,
        "resource-exclusive nodes overlapped"
    );
}

#[tokio::test]
async fn different_resources_still_overlap() {
    let h = Harness::new().await;
    let body = h.probe("both", "seen.txt");
    let src = agent_tasks_with_resource(&[("a", "pg", &body), ("b", "redis", &body)]);

    assert_eq!(h.run(&src, 4).await.status, RunStatus::Ok);
    assert_eq!(h.peak("seen.txt"), 2, "distinct resources should not block");
}

#[tokio::test]
async fn writes_a_log_file_per_node() {
    let h = Harness::new().await;
    let src = agent_tasks(&[("a", "echo marker")]);
    let graph = parse_graph(&src).unwrap();
    let dag = Dag::build(&graph.tasks).unwrap();
    let paths = create_run(&runs_root(h.tmp.path()), 9).unwrap();
    let mut log = EventLog::open_append(paths.events()).unwrap();
    let mut state = RunState::new(dag.ids());
    let opts = RunOpts {
        jobs: 1,
        cwd: h.repo.clone(),
        cancel: CancellationToken::new(),
        repo: Some(h.repo.clone()),
        seed_from: h.repo.clone(),
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
    let h = Harness::new().await;
    let out = h.run(&agent_tasks(&[("a", "true")]), 1).await;

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
    let tmp = tempfile::tempdir().unwrap();
    let src = "[providers.p]\ncmd=\"true\"\n\
               [[task]]\nid=\"a\"\nprompt=\"hi\"\nprovider=\"p\"\nverify=\"true\"\n";
    let graph = parse_graph(src).unwrap();
    let dag = Dag::build(&graph.tasks).unwrap();
    let paths = create_run(&runs_root(tmp.path()), 1).unwrap();
    let mut log = EventLog::open_append(paths.events()).unwrap();
    let mut state = RunState::new(dag.ids());
    let opts = RunOpts {
        jobs: 1,
        cwd: tmp.path().to_path_buf(),
        cancel: CancellationToken::new(),
        repo: None,
        seed_from: tmp.path().to_path_buf(),
        remote: workspace::DEFAULT_REMOTE.to_string(),
    };

    let status = execute(&graph, &dag, &paths, &mut log, &mut state, &opts)
        .await
        .unwrap();
    let events: Vec<EventKind> = EventLog::read(paths.events())
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect();

    assert_eq!(status, RunStatus::Partial);
    assert_eq!(state.state("a"), NodeState::Failed);
    assert!(
        events.iter().any(
            |k| matches!(k, EventKind::NodeFailed { reason, .. } if reason.contains("repository"))
        ),
        "{events:?}"
    );
}

#[tokio::test]
async fn an_empty_graph_is_ok() {
    let tmp = tempfile::tempdir().unwrap();
    let graph = parse_graph("").unwrap();
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
    assert_eq!(status, RunStatus::Ok);
}
