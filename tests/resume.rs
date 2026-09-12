use assembly_line::config::parse_graph;
use assembly_line::dag::Dag;
use assembly_line::event::{EventKind, EventLog, RunStatus};
use assembly_line::git::{self, commit_all};
use assembly_line::paths::{create_run, repo_worktrees_root, runs_root};
use assembly_line::scheduler::{RunOpts, execute};
use assembly_line::state::{NodeState, RunState};
use assembly_line::workspace;
use assert_cmd::Command;
use predicates::str::contains;
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

/// A repository with one commit, so agent nodes have somewhere to branch
/// from. Every task is an agent task now, so a resumed run needs one too.
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

    fn opts(&self, jobs: usize) -> RunOpts {
        RunOpts {
            jobs,
            cwd: self.repo.clone(),
            cancel: CancellationToken::new(),
            repo: Some(self.repo.clone()),
            seed_from: self.repo.clone(),
            remote: workspace::DEFAULT_REMOTE.to_string(),
        }
    }
}

/// A chain whose first node is always marked complete by hand before a test
/// even starts — resume must not run it again — while the rest run for real
/// through a trivial provider.
const CHAIN: &str = r#"
[providers.run]
cmd = "bash"
args = ["-c", "{prompt}"]

[[task]]
id = "one"
prompt = "unused — this node is never actually launched"

[[task]]
id = "two"
needs = ["one"]
provider = "run"
prompt = "true"

[[task]]
id = "three"
needs = ["two"]
provider = "run"
prompt = "true"
"#;

#[tokio::test]
async fn resume_does_not_rerun_completed_nodes() {
    let h = Harness::new().await;
    let graph = parse_graph(CHAIN).unwrap();
    let dag = Dag::build(&graph.tasks).unwrap();
    let paths = create_run(&runs_root(h.tmp.path()), 1).unwrap();

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

    let events = EventLog::read(paths.events()).unwrap();
    let mut state = RunState::replay(dag.ids(), &events);
    assert_eq!(state.state("two"), NodeState::Running);
    assert_eq!(state.reset_running(), vec!["two".to_string()]);

    let mut log = EventLog::open_append(paths.events()).unwrap();
    let opts = h.opts(4);
    let status = execute(&graph, &dag, &paths, &mut log, &mut state, &opts)
        .await
        .unwrap();

    assert_eq!(status, RunStatus::Ok);
    assert_eq!(state.state("two"), NodeState::Done);
    assert_eq!(state.state("three"), NodeState::Done);

    let all_events = EventLog::read(paths.events()).unwrap();
    let one_starts = all_events
        .iter()
        .filter(|e| matches!(&e.kind, EventKind::NodeStarted { node, .. } if node == "one"))
        .count();
    assert_eq!(one_starts, 1, "'one' must not run twice");
}

#[tokio::test]
async fn resume_appends_to_the_same_log() {
    let h = Harness::new().await;
    let graph = parse_graph(CHAIN).unwrap();
    let dag = Dag::build(&graph.tasks).unwrap();
    let paths = create_run(&runs_root(h.tmp.path()), 1).unwrap();

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
    let opts = h.opts(4);
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

/// Where this test's worktrees go: outside the repository, and outside the
/// shared `$HOME` default. `gc` walks every repository it can see, so tests
/// sharing one root would collect each other's work mid-run.
fn worktree_root_for(tmp: &tempfile::TempDir) -> PathBuf {
    std::env::temp_dir().join(format!(
        "assembly-test-wt-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    ))
}

fn assembly(tmp: &tempfile::TempDir) -> Command {
    let mut cmd = Command::cargo_bin("assembly").unwrap();
    cmd.current_dir(tmp.path());
    cmd.env(
        assembly_line::paths::WORKTREE_ROOT_VAR,
        worktree_root_for(tmp),
    );
    cmd
}

fn discard_worktrees(tmp: &tempfile::TempDir) {
    let _ = std::fs::remove_dir_all(worktree_root_for(tmp));
}

#[test]
fn resume_reports_the_node_it_requeued() {
    let tmp = tempfile::tempdir().unwrap();
    for args in [
        vec!["init", "-q", "--initial-branch=main"],
        vec!["config", "user.email", "t@e.com"],
        vec!["config", "user.name", "T"],
        vec!["config", "commit.gpgsign", "false"],
    ] {
        std::process::Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(&args)
            .status()
            .unwrap();
    }
    std::fs::write(tmp.path().join("README.md"), "base\n").unwrap();
    std::process::Command::new("git")
        .args(["-C", tmp.path().to_str().unwrap(), "add", "-A"])
        .status()
        .unwrap();
    std::process::Command::new("git")
        .args(["-C", tmp.path().to_str().unwrap(), "commit", "-qm", "init"])
        .status()
        .unwrap();
    std::fs::write(
        tmp.path().join("graph.toml"),
        "[providers.p]\ncmd=\"bash\"\nargs=[\"-c\",\"true\"]\n\
         [[task]]\nid=\"a\"\nprovider=\"p\"\nprompt=\"x\"\n",
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

    assembly(&tmp)
        .args(["resume", "1"])
        .assert()
        .success()
        .stderr(contains("'a' was in flight"));

    discard_worktrees(&tmp);
}
