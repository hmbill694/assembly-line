use assembly_line::config::parse_graph;
use assembly_line::dag::Dag;
use assembly_line::event::{EventKind, EventLog, RunStatus};
use assembly_line::git::{self, commit_all};
use assembly_line::paths::{create_run, repo_worktrees_root, runs_root};
use assembly_line::scheduler::{RunOpts, execute};
use assembly_line::state::{NodeState, RunState};
use assembly_line::workspace;
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

struct Outcome {
    status: RunStatus,
    state: RunState,
    events: Vec<EventKind>,
}

/// A repository with one commit, so agent nodes have somewhere to branch
/// from. Every task is an agent task now, so every failure-handling test
/// needs one.
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
}

/// One `[providers.run]` block whose command is the task's own prompt, run
/// through a shell. Lets a test hand each node an arbitrary script without
/// needing a fixture file for it.
const RUN_PROVIDER: &str = "[providers.run]\ncmd = \"bash\"\nargs = [\"-c\", \"{prompt}\"]\n";

async fn run(h: &Harness, src: &str) -> Outcome {
    let graph = parse_graph(src).unwrap();
    let dag = Dag::build(&graph.tasks).unwrap();
    let paths = create_run(&runs_root(h.tmp.path()), 1).unwrap();
    let mut log = EventLog::open_append(paths.events()).unwrap();
    let mut state = RunState::new(dag.ids());
    let opts = RunOpts {
        jobs: 4,
        cwd: h.repo.clone(),
        cancel: CancellationToken::new(),
        repo: Some(h.repo.clone()),
        seed_from: h.repo.clone(),
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
    }
}

#[tokio::test]
async fn a_failure_skips_its_subtree_and_spares_independent_branches() {
    let h = Harness::new().await;
    let src = format!(
        "{RUN_PROVIDER}\n\
[[task]]
id = \"build\"
provider = \"run\"
prompt = \"true\"

[[task]]
id = \"impl-auth\"
needs = [\"build\"]
provider = \"run\"
prompt = \"exit 1\"

[[task]]
id = \"wire-routes\"
needs = [\"impl-auth\"]
provider = \"run\"
prompt = \"true\"

[[task]]
id = \"impl-api\"
needs = [\"build\"]
provider = \"run\"
prompt = \"true\"
"
    );
    let out = run(&h, &src).await;

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
    let h = Harness::new().await;
    let src = format!(
        "{RUN_PROVIDER}\n\
[[task]]
id = \"a\"
provider = \"run\"
prompt = \"exit 1\"

[[task]]
id = \"b\"
needs = [\"a\"]
provider = \"run\"
prompt = \"true\"

[[task]]
id = \"c\"
needs = [\"b\"]
provider = \"run\"
prompt = \"true\"
"
    );
    let out = run(&h, &src).await;

    assert_eq!(out.state.state("b"), NodeState::Skipped);
    assert_eq!(out.state.state("c"), NodeState::Skipped);
}

#[tokio::test]
async fn on_failure_continue_lets_dependents_run() {
    let h = Harness::new().await;
    let src = format!(
        "{RUN_PROVIDER}\n\
[[task]]
id = \"lint\"
provider = \"run\"
prompt = \"exit 1\"
on_failure = \"continue\"

[[task]]
id = \"build\"
needs = [\"lint\"]
provider = \"run\"
prompt = \"true\"
"
    );
    let out = run(&h, &src).await;

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
    let h = Harness::new().await;
    let src = format!(
        "{RUN_PROVIDER}\n\
[[task]]
id = \"migrate\"
provider = \"run\"
prompt = \"exit 1\"
on_failure = \"abort\"

[[task]]
id = \"slow\"
provider = \"run\"
prompt = \"sleep 5\"

[[task]]
id = \"later\"
needs = [\"migrate\"]
provider = \"run\"
prompt = \"true\"
"
    );
    let started = std::time::Instant::now();
    let out = run(&h, &src).await;

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
    let h = Harness::new().await;
    let src = format!(
        "{RUN_PROVIDER}\n\
[[task]]
id = \"hang\"
provider = \"run\"
prompt = \"sleep 10\"
max_duration = \"200ms\"
"
    );
    let out = run(&h, &src).await;

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
    let h = Harness::new().await;
    let src = format!("{RUN_PROVIDER}\n[[task]]\nid=\"a\"\nprovider=\"run\"\nprompt=\"exit 7\"\n");
    let out = run(&h, &src).await;
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
    let h = Harness::new().await;
    let src = format!("{RUN_PROVIDER}\n[[task]]\nid=\"a\"\nprovider=\"run\"\nprompt=\"true\"\n");
    let out = run(&h, &src).await;
    assert_eq!(out.status, RunStatus::Ok);
}
