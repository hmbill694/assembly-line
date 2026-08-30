use assembly_line::config::parse_graph;
use assembly_line::dag::Dag;
use assembly_line::event::{EventKind, EventLog, RunStatus};
use assembly_line::git::{self, commit_all, head_sha};
use assembly_line::paths::{create_run, repo_worktrees_root, runs_root};
use assembly_line::scheduler::{RunOpts, execute};
use assembly_line::state::{NodeState, RunState};
use assembly_line::workspace::{self, run_branch_name};
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// A `[providers.fake]` block that runs one of the fixture scripts.
/// `extra_arg` reaches the script as `$2`, distinguishing two instances.
fn provider_block(script: &str, extra_arg: &str) -> String {
    format!(
        "[providers.fake]\ncmd = \"bash\"\nargs = [\"{}\", \"{{prompt}}\", \"{extra_arg}\"]\n",
        fixture(script).display()
    )
}

struct Harness {
    tmp: tempfile::TempDir,
    repo: PathBuf,
}

/// Worktrees live under `$HOME`. Jobs discard their own, but a run that dies
/// mid-node can still orphan one, and tests must not leave that behind.
impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(root) = repo_worktrees_root(&self.repo) {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

struct Outcome {
    status: RunStatus,
    state: RunState,
    events: Vec<EventKind>,
    run_id: u64,
}

impl Outcome {
    fn has(&self, predicate: impl Fn(&EventKind) -> bool) -> bool {
        self.events.iter().any(predicate)
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

    /// Add a bare repository as `origin`, so a publish exercises a real push
    /// with no network and no credentials.
    async fn with_origin(&self) -> PathBuf {
        let origin = self.tmp.path().join("origin.git");
        let origin_arg = origin.to_string_lossy().into_owned();

        for args in [
            vec!["init", "--bare", "--initial-branch=main", &origin_arg],
            vec!["remote", "add", "origin", &origin_arg],
        ] {
            let out = git::run_allowing_failure(&self.repo, &args).await.unwrap();
            assert!(out.succeeded(), "git {args:?} failed: {}", out.stderr);
        }
        origin
    }

    async fn run(&self, src: &str, jobs: usize) -> Outcome {
        let graph = parse_graph(src).unwrap();
        let dag = Dag::build(&graph.tasks).unwrap();
        let run = create_run(&runs_root(self.tmp.path()), 1).unwrap();
        let mut log = EventLog::open_append(run.events()).unwrap();
        let mut state = RunState::new(dag.ids());

        let opts = RunOpts {
            jobs,
            cwd: self.repo.clone(),
            cancel: CancellationToken::new(),
            repo: Some(self.repo.clone()),
            seed_from: self.repo.clone(),
            remote: workspace::DEFAULT_REMOTE.to_string(),
        };
        let status = execute(&graph, &dag, &run, &mut log, &mut state, &opts)
            .await
            .unwrap();

        let events = EventLog::read(run.events())
            .unwrap()
            .into_iter()
            .map(|e| e.kind)
            .collect();

        Outcome {
            status,
            state,
            events,
            run_id: run.id,
        }
    }
}

#[tokio::test]
async fn an_agent_node_runs_commits_and_merges_into_the_run_branch() {
    let h = Harness::new().await;
    let base = head_sha(&h.repo).await.unwrap();
    let src = format!(
        "{}\n[[task]]\nid = \"impl-auth\"\nkind = \"agent\"\nprovider = \"fake\"\n\
         prompt = \"add authentication\"\n",
        provider_block("fake-agent.sh", "a")
    );

    let out = h.run(&src, 1).await;

    assert_eq!(out.status, RunStatus::Ok);
    assert_eq!(out.state.state("impl-auth"), NodeState::Done);
    assert!(out.has(|k| matches!(k, EventKind::NodeCommitted { node, .. } if node == "impl-auth")));
    assert!(out.has(|k| matches!(k, EventKind::NodeMerged { node, .. } if node == "impl-auth")));

    // The user's checkout and branch are untouched.
    assert_eq!(head_sha(&h.repo).await.unwrap(), base);
    assert_eq!(
        git::current_branch(&h.repo).await.unwrap().as_deref(),
        Some("main")
    );
    assert!(!h.repo.join("agent-output.txt").exists());
    assert!(!git::is_dirty(&h.repo).await.unwrap());

    // The work is on the run branch.
    let branch = run_branch_name(out.run_id);
    assert!(git::branch_exists(&h.repo, &branch).await.unwrap());
    let listed = git::run_allowing_failure(&h.repo, &["ls-tree", "--name-only", &branch])
        .await
        .unwrap()
        .stdout;
    assert!(listed.contains("agent-output.txt"), "{listed}");
}

#[tokio::test]
async fn the_prompt_reaches_the_agent_intact() {
    let h = Harness::new().await;
    let src = format!(
        "{}\n[[task]]\nid = \"n\"\nkind = \"agent\"\nprovider = \"fake\"\n\
         prompt = \"quotes \\\" and $HOME and ; semicolons\"\n",
        provider_block("fake-agent.sh", "a")
    );

    let out = h.run(&src, 1).await;
    assert_eq!(out.status, RunStatus::Ok);

    let content = git::run_allowing_failure(
        &h.repo,
        &[
            "show",
            &format!("{}:agent-output.txt", run_branch_name(out.run_id)),
        ],
    )
    .await
    .unwrap()
    .stdout;
    assert!(
        content.contains("$HOME"),
        "the shell expanded the prompt: {content}"
    );
    assert!(content.contains("; semicolons"), "{content}");
}

/// The job contract: a node's branch always survives, its worktree never does.
/// A half-finished failure is exactly the case where the diff is worth
/// reading, so the work is committed before the failure is judged.
#[tokio::test]
async fn a_failing_agent_preserves_its_work_on_a_branch_and_leaves_no_worktree() {
    let h = Harness::new().await;
    let src = format!(
        "{}\n[[task]]\nid = \"broken\"\nkind = \"agent\"\nprovider = \"fake\"\nprompt = \"x\"\n",
        provider_block("failing-agent.sh", "a")
    );

    let out = h.run(&src, 1).await;

    assert_eq!(out.status, RunStatus::Partial);
    assert_eq!(out.state.state("broken"), NodeState::Failed);
    assert!(
        out.has(|k| matches!(k, EventKind::NodeFailed { reason, .. } if reason.contains("exit 3")))
    );
    // Preserved, but never merged: failed work does not reach the run branch.
    assert!(!out.has(|k| matches!(k, EventKind::NodeMerged { .. })));
    assert!(out.has(|k| matches!(k, EventKind::NodeCommitted { node, .. } if node == "broken")));
    assert!(out.has(
        |k| matches!(k, EventKind::NodeBranchPublished { branch, .. } if branch == "al/run-1-broken")
    ));

    let scratch = assembly_line::paths::worktree_root(&h.repo, out.run_id)
        .unwrap()
        .join("broken");
    assert!(
        !scratch.exists(),
        "a job's checkout is scratch and must not outlive it"
    );

    // The work is on the branch, which is what makes the failure inspectable
    // from anywhere rather than only on the machine that ran it.
    let on_branch = git::run_allowing_failure(
        &h.repo,
        &["show", "--name-only", "--format=", "al/run-1-broken"],
    )
    .await
    .unwrap();
    assert!(on_branch.succeeded(), "{}", on_branch.stderr);
    assert!(
        on_branch.stdout.contains("partial.txt"),
        "the agent's partial work is not on the branch: {}",
        on_branch.stdout
    );
}

/// A gate with nobody at it defers rather than blocks: the node merges on its
/// own and is recorded as waiting to be looked at. A run never stalls.
#[tokio::test]
async fn a_gated_node_defers_its_gate_instead_of_blocking() {
    let h = Harness::new().await;
    let src = format!(
        "{}\n[[task]]\nid = \"work\"\nkind = \"agent\"\nprovider = \"fake\"\n\
         prompt = \"x\"\nsupervise = \"on-complete\"\n",
        provider_block("fake-agent.sh", "a")
    );

    let out = h.run(&src, 1).await;

    assert_eq!(out.status, RunStatus::Ok);
    assert_eq!(out.state.state("work"), NodeState::Done);
    assert!(out.has(|k| matches!(k, EventKind::NodeMerged { .. })));
    assert!(out.has(|k| matches!(k, EventKind::NodeAwaitingReview { node } if node == "work")));
}

/// The gate is the node's own declaration, so an ungated node never enters the
/// inbox — otherwise every shell node would need reviewing.
#[tokio::test]
async fn an_ungated_node_is_never_awaiting_review() {
    let h = Harness::new().await;
    let src = format!(
        "{}\n[[task]]\nid = \"work\"\nkind = \"agent\"\nprovider = \"fake\"\nprompt = \"x\"\n",
        provider_block("fake-agent.sh", "a")
    );

    let out = h.run(&src, 1).await;

    assert!(out.has(|k| matches!(k, EventKind::NodeMerged { .. })));
    assert!(!out.has(|k| matches!(k, EventKind::NodeAwaitingReview { .. })));
}

/// A failed node is awaiting a fix, not a review — it never merged, so there
/// is nothing at the gate to accept.
#[tokio::test]
async fn a_gated_node_that_failed_is_not_awaiting_review() {
    let h = Harness::new().await;
    let src = format!(
        "{}\n[[task]]\nid = \"broken\"\nkind = \"agent\"\nprovider = \"fake\"\n\
         prompt = \"x\"\nsupervise = \"on-complete\"\n",
        provider_block("failing-agent.sh", "a")
    );

    let out = h.run(&src, 1).await;

    assert_eq!(out.state.state("broken"), NodeState::Failed);
    assert!(!out.has(|k| matches!(k, EventKind::NodeAwaitingReview { .. })));
}

/// The point of publishing: a failed node's work leaves the machine that ran
/// it. This is what a container or a k8s Job will rely on in M4.
#[tokio::test]
async fn a_failed_nodes_branch_reaches_the_remote() {
    let h = Harness::new().await;
    let origin = h.with_origin().await;
    let src = format!(
        "{}\n[[task]]\nid = \"broken\"\nkind = \"agent\"\nprovider = \"fake\"\nprompt = \"x\"\n",
        provider_block("failing-agent.sh", "a")
    );

    let out = h.run(&src, 1).await;

    assert_eq!(out.state.state("broken"), NodeState::Failed);
    assert!(out.has(
        |k| matches!(k, EventKind::NodeBranchPublished { pushed_to, .. } if pushed_to.as_deref() == Some("origin"))
    ));

    let on_remote = git::run_allowing_failure(
        &origin,
        &["show", "--name-only", "--format=", "al/run-1-broken"],
    )
    .await
    .unwrap();
    assert!(on_remote.succeeded(), "{}", on_remote.stderr);
    assert!(
        on_remote.stdout.contains("partial.txt"),
        "the failed node's work never reached the remote: {}",
        on_remote.stdout
    );
}

/// With no remote configured the branch simply stays local. That is a complete
/// outcome, not a degraded one, so it is still recorded as published.
#[tokio::test]
async fn publishing_without_a_remote_keeps_the_branch_local() {
    let h = Harness::new().await;
    let src = format!(
        "{}\n[[task]]\nid = \"work\"\nkind = \"agent\"\nprovider = \"fake\"\nprompt = \"x\"\n",
        provider_block("fake-agent.sh", "a")
    );

    let out = h.run(&src, 1).await;

    assert_eq!(out.status, RunStatus::Ok);
    assert!(out.has(
        |k| matches!(k, EventKind::NodeBranchPublished { pushed_to, .. } if pushed_to.is_none())
    ));
}

#[tokio::test]
async fn an_agent_that_changes_nothing_succeeds_without_a_merge() {
    let h = Harness::new().await;
    let src = format!(
        "{}\n[[task]]\nid = \"n\"\nkind = \"agent\"\nprovider = \"fake\"\nprompt = \"x\"\n",
        provider_block("noop-agent.sh", "a")
    );

    let out = h.run(&src, 1).await;

    assert_eq!(out.status, RunStatus::Ok);
    assert_eq!(out.state.state("n"), NodeState::Done);
    assert!(!out.has(|k| matches!(k, EventKind::NodeCommitted { .. })));
    assert!(!out.has(|k| matches!(k, EventKind::NodeMerged { .. })));
}

#[tokio::test]
async fn a_second_node_sees_the_first_nodes_merged_work() {
    let h = Harness::new().await;
    let src = format!(
        "{}\n\
         [[task]]\nid = \"first\"\nkind = \"agent\"\nprovider = \"fake\"\nprompt = \"one\"\n\
         [[task]]\nid = \"second\"\nkind = \"shell\"\nneeds = [\"first\"]\n\
         run = \"test -f agent-output.txt\"\n",
        provider_block("fake-agent.sh", "a")
    );

    let out = h.run(&src, 1).await;

    assert_eq!(
        out.state.state("second"),
        NodeState::Done,
        "a dependent must run against the merged run branch, not the pristine repo"
    );
    assert_eq!(out.status, RunStatus::Ok);
}

#[tokio::test]
async fn two_agents_editing_the_same_file_conflict_on_the_second_merge() {
    let h = Harness::new().await;
    let src = format!(
        "[providers.a]\ncmd = \"bash\"\nargs = [\"{script}\", \"{{prompt}}\", \"a\"]\n\
         [providers.b]\ncmd = \"bash\"\nargs = [\"{script}\", \"{{prompt}}\", \"b\"]\n\
         [[task]]\nid = \"one\"\nkind = \"agent\"\nprovider = \"a\"\nprompt = \"x\"\n\
         [[task]]\nid = \"two\"\nkind = \"agent\"\nprovider = \"b\"\nprompt = \"y\"\n",
        script = fixture("conflicting-agent.sh").display()
    );

    let out = h.run(&src, 2).await;

    assert_eq!(out.status, RunStatus::Partial);
    assert!(
        out.has(
            |k| matches!(k, EventKind::NodeMergeConflicted { paths, .. } if paths.contains(&"shared.txt".to_string()))
        ),
        "{:?}",
        out.events
    );

    let done = [out.state.state("one"), out.state.state("two")];
    assert!(
        done.contains(&NodeState::Done) && done.contains(&NodeState::Failed),
        "exactly one should land and one should conflict: {done:?}"
    );
}

#[tokio::test]
async fn seeded_files_reach_the_agent_but_never_the_run_branch() {
    let h = Harness::new().await;
    std::fs::write(h.repo.join(".env"), "API_KEY=hunter2\n").unwrap();
    let src = format!(
        "{}\n[workspace]\ncopy = [\".env\"]\n\
         [[task]]\nid = \"n\"\nkind = \"agent\"\nprovider = \"fake\"\nprompt = \"x\"\n",
        provider_block("fake-agent.sh", "a")
    );

    let out = h.run(&src, 1).await;
    assert_eq!(out.status, RunStatus::Ok);

    let listed = git::run_allowing_failure(
        &h.repo,
        &["ls-tree", "--name-only", "-r", &run_branch_name(out.run_id)],
    )
    .await
    .unwrap()
    .stdout;
    assert!(
        !listed.contains(".env"),
        "the seeded secret reached a branch: {listed}"
    );
}

#[tokio::test]
async fn a_shell_only_graph_creates_no_branch_and_no_worktrees() {
    let h = Harness::new().await;
    let out = h
        .run(
            "[[task]]\nid = \"a\"\nkind = \"shell\"\nrun = \"true\"\n",
            1,
        )
        .await;

    assert_eq!(out.status, RunStatus::Ok);
    assert!(!out.has(|k| matches!(k, EventKind::RunBranchCreated { .. })));
    assert!(
        !git::branch_exists(&h.repo, &run_branch_name(out.run_id))
            .await
            .unwrap(),
        "a shell-only run must behave exactly as it did in M1"
    );
    assert!(
        !assembly_line::paths::worktree_root(&h.repo, out.run_id)
            .unwrap()
            .exists()
    );
}

#[tokio::test]
async fn a_missing_provider_binary_fails_the_node_with_a_useful_message() {
    let h = Harness::new().await;
    let src = "[providers.gone]\ncmd = \"definitely-not-real-xyz\"\nargs = [\"{prompt}\"]\n\
               [[task]]\nid = \"n\"\nkind = \"agent\"\nprovider = \"gone\"\nprompt = \"x\"\n";

    let out = h.run(src, 1).await;

    assert_eq!(out.status, RunStatus::Partial);
    assert!(
        out.has(
            |k| matches!(k, EventKind::NodeFailed { reason, .. } if reason.contains("definitely-not-real-xyz"))
        ),
        "{:?}",
        out.events
    );
}
