use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;

/// A directory that looks like a git repo, so run state has somewhere to live.
fn repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
    tmp
}

fn with_graph(body: &str) -> tempfile::TempDir {
    let tmp = repo();
    std::fs::write(tmp.path().join("graph.toml"), body).unwrap();
    tmp
}

/// Where this test's worktrees go: outside the repository, and outside the
/// shared `$HOME` default. `gc` walks every repository it can see, so tests
/// sharing one root would collect each other's work mid-run.
fn worktree_root_for(tmp: &tempfile::TempDir) -> std::path::PathBuf {
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

#[test]
fn validate_accepts_a_good_graph() {
    let tmp = with_graph("[[task]]\nid=\"a\"\nprompt=\"x\"\n");
    assembly(&tmp)
        .args(["validate", "graph.toml"])
        .assert()
        .success()
        .stdout(contains("ok (1 tasks)"));
}

#[test]
fn validate_reports_a_cycle_and_exits_two() {
    let tmp = with_graph(
        "[[task]]\nid=\"a\"\nneeds=[\"b\"]\nprompt=\"x\"\n\
         [[task]]\nid=\"b\"\nneeds=[\"a\"]\nprompt=\"x\"\n",
    );
    assembly(&tmp)
        .args(["validate", "graph.toml"])
        .assert()
        .code(2)
        .stderr(contains("dependency cycle"));
}

#[test]
fn validate_warns_about_an_agent_without_verify() {
    let tmp = with_graph(
        "[providers.p]\ncmd=\"true\"\n\
         [[task]]\nid=\"a\"\nprompt=\"hi\"\nprovider=\"p\"\n",
    );
    assembly(&tmp)
        .args(["validate", "graph.toml"])
        .assert()
        .success()
        .stderr(contains("nothing will check its output"));
}

#[test]
fn run_exits_zero_and_records_the_run() {
    let tmp = repo_with_commit();
    std::fs::write(tmp.path().join("graph.toml"), agent_graph("fake-agent.sh")).unwrap();
    assembly(&tmp)
        .args(["run", "graph.toml"])
        .assert()
        .success()
        .stdout(contains("run 1").and(contains("ok")));

    assert!(tmp.path().join(".assembly/runs/1/events.jsonl").is_file());
    assert!(tmp.path().join(".assembly/runs/1/meta.json").is_file());

    discard_worktrees(&tmp);
}

#[test]
fn run_exits_one_when_a_node_fails() {
    let tmp = repo_with_commit();
    std::fs::write(
        tmp.path().join("graph.toml"),
        agent_graph("failing-agent.sh"),
    )
    .unwrap();
    assembly(&tmp)
        .args(["run", "graph.toml"])
        .assert()
        .code(1)
        .stdout(contains("partial"));

    discard_worktrees(&tmp);
}

#[test]
fn run_exits_two_on_an_invalid_graph_and_leaves_no_run_directory() {
    let tmp = with_graph("[[task]]\nid=\"a\"\nneeds=[\"ghost\"]\nprompt=\"x\"\n");
    assembly(&tmp)
        .args(["run", "graph.toml"])
        .assert()
        .code(2)
        .stderr(contains("ghost"));

    assert!(
        !tmp.path().join(".assembly/runs/1").exists(),
        "a rejected graph should not allocate a run directory"
    );
}

#[test]
fn run_outside_a_git_repo_explains_itself() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("graph.toml"),
        "[[task]]\nid=\"a\"\nprompt=\"x\"\n",
    )
    .unwrap();

    Command::cargo_bin("assembly")
        .unwrap()
        .current_dir(tmp.path())
        .args(["run", "graph.toml"])
        .assert()
        .code(2)
        .stderr(contains("not inside a git repository"));
}

#[test]
fn status_and_logs_report_a_finished_run() {
    let tmp = repo_with_commit();
    std::fs::write(
        tmp.path().join("graph.toml"),
        "[providers.run]\ncmd = \"bash\"\nargs = [\"-c\", \"{prompt}\"]\n\
         [[task]]\nid=\"build\"\nprovider=\"run\"\nprompt=\"echo building\"\n\
         [[task]]\nid=\"broken\"\nneeds=[\"build\"]\nprovider=\"run\"\nprompt=\"echo nope 1>&2; exit 1\"\n\
         [[task]]\nid=\"after\"\nneeds=[\"broken\"]\nprovider=\"run\"\nprompt=\"true\"\n",
    )
    .unwrap();

    assembly(&tmp).args(["run", "graph.toml"]).assert().code(1);

    // No run id given — defaults to the most recent run.
    assembly(&tmp)
        .arg("status")
        .assert()
        .success()
        .stdout(
            contains("build")
                .and(contains("broken"))
                .and(contains("after")),
        )
        .stdout(contains("exit 1").and(contains("needs broken")))
        .stdout(contains("partial"));

    assembly(&tmp)
        .args(["logs", "1", "broken"])
        .assert()
        .success()
        .stdout(contains("nope"));

    discard_worktrees(&tmp);
}

#[test]
fn logs_for_an_unknown_node_explains_itself() {
    let tmp = repo_with_commit();
    std::fs::write(tmp.path().join("graph.toml"), agent_graph("fake-agent.sh")).unwrap();
    assembly(&tmp)
        .args(["run", "graph.toml"])
        .assert()
        .success();

    assembly(&tmp)
        .args(["logs", "1", "ghost"])
        .assert()
        .code(2)
        .stderr(contains("no log for node 'ghost'"));

    discard_worktrees(&tmp);
}

#[test]
fn status_with_no_runs_explains_itself() {
    let tmp = repo();
    assembly(&tmp)
        .arg("status")
        .assert()
        .code(2)
        .stderr(contains("no runs yet"));
}

#[test]
fn resume_of_an_unknown_run_explains_itself() {
    let tmp = repo();
    assembly(&tmp)
        .args(["resume", "42"])
        .assert()
        .code(2)
        .stderr(contains("no such run: 42"));
}

#[test]
fn a_prompt_file_is_read_through_the_cli() {
    let tmp = repo();
    std::fs::write(tmp.path().join("auth.md"), "Implement auth").unwrap();
    std::fs::write(
        tmp.path().join("graph.toml"),
        "[providers.p]\ncmd=\"true\"\n\
         [[task]]\nid=\"a\"\nprovider=\"p\"\nprompt_file=\"auth.md\"\nverify=\"true\"\n",
    )
    .unwrap();

    assembly(&tmp)
        .args(["validate", "graph.toml"])
        .assert()
        .success();
}

#[test]
fn a_missing_prompt_file_is_reported_before_the_run_starts() {
    let tmp = repo();
    std::fs::write(
        tmp.path().join("graph.toml"),
        "[providers.p]\ncmd=\"true\"\n\
         [[task]]\nid=\"a\"\nprovider=\"p\"\nprompt_file=\"gone.md\"\nverify=\"true\"\n",
    )
    .unwrap();

    assembly(&tmp)
        .args(["run", "graph.toml"])
        .assert()
        .code(2)
        .stderr(contains("gone.md"));

    assert!(!tmp.path().join(".assembly/runs/1").exists());
}

/// A repo with a real commit, which a run branch requires.
fn repo_with_commit() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let at = tmp.path().to_str().unwrap();
    for args in [
        vec!["-C", at, "init", "-q", "--initial-branch=main"],
        vec!["-C", at, "config", "user.email", "t@e.com"],
        vec!["-C", at, "config", "user.name", "T"],
        vec!["-C", at, "config", "commit.gpgsign", "false"],
        vec!["-C", at, "add", "-A"],
    ] {
        std::process::Command::new("git")
            .args(&args)
            .status()
            .unwrap();
    }
    std::fs::write(tmp.path().join("README.md"), "base\n").unwrap();
    for args in [
        vec!["-C", at, "add", "-A"],
        vec!["-C", at, "commit", "-qm", "init"],
    ] {
        std::process::Command::new("git")
            .args(&args)
            .status()
            .unwrap();
    }
    tmp
}

/// The repository path the binary itself will see. On macOS a tempdir sits
/// under a symlink, so the slug must be computed from the resolved path.
fn as_the_binary_sees_it(tmp: &tempfile::TempDir) -> std::path::PathBuf {
    std::fs::canonicalize(tmp.path()).unwrap()
}

/// Where the binary will put run `id`'s worktrees, given the root this test
/// hands it.
fn run_worktrees(tmp: &tempfile::TempDir, id: u64) -> std::path::PathBuf {
    worktree_root_for(tmp)
        .join(assembly_line::paths::repo_slug(&as_the_binary_sees_it(tmp)))
        .join(id.to_string())
}

/// Worktrees outlive the tempdir, so a test that starts an agent node has to
/// take them with it.
fn discard_worktrees(tmp: &tempfile::TempDir) {
    let _ = std::fs::remove_dir_all(worktree_root_for(tmp));
}

fn agent_graph(script: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(script);
    format!(
        "[providers.fake]\ncmd = \"bash\"\nargs = [\"{}\", \"{{prompt}}\", \"x\"]\n\
         [[task]]\nid = \"n\"\nprovider = \"fake\"\nprompt = \"hi\"\n",
        path.display()
    )
}

#[test]
fn gc_with_nothing_to_collect_says_so() {
    let tmp = repo_with_commit();
    std::fs::write(tmp.path().join("graph.toml"), agent_graph("fake-agent.sh")).unwrap();
    assembly(&tmp)
        .args(["run", "graph.toml"])
        .assert()
        .success();

    assembly(&tmp)
        .args(["gc", "--dry-run"])
        .assert()
        .success()
        .stdout(contains("nothing to collect"));

    discard_worktrees(&tmp);
}

#[test]
fn gc_collects_a_runs_worktrees_only_once_its_run_state_is_gone() {
    let tmp = repo_with_commit();
    std::fs::write(
        tmp.path().join("graph.toml"),
        agent_graph("failing-agent.sh"),
    )
    .unwrap();

    assembly(&tmp).args(["run", "graph.toml"]).assert().code(1);

    let worktrees = run_worktrees(&tmp, 1);
    assert!(
        !worktrees.join("n").exists(),
        "a job's checkout is scratch — even a failed one discards it"
    );

    // The run still exists, so its worktrees are still wanted.
    assembly(&tmp)
        .args(["gc"])
        .assert()
        .success()
        .stdout(contains("nothing to collect"));
    assert!(worktrees.exists());

    std::fs::remove_dir_all(tmp.path().join(".assembly")).unwrap();

    assembly(&tmp)
        .args(["gc", "--dry-run"])
        .assert()
        .success()
        .stdout(contains("would remove").and(contains("no state directory")));
    assert!(worktrees.exists(), "--dry-run removed something");

    assembly(&tmp)
        .args(["gc"])
        .assert()
        .success()
        .stdout(contains("removed 1"));
    assert!(!worktrees.exists());

    discard_worktrees(&tmp);
}

/// The heart of the stateless design: a revise is a new job, and the agent
/// sees its prior work because that work *is* the branch it starts from.
/// Nothing was kept on disk between the two rounds.
#[test]
fn a_revise_round_continues_the_branch_instead_of_starting_over() {
    let tmp = repo_with_commit();
    std::fs::write(
        tmp.path().join("graph.toml"),
        agent_graph("revising-agent.sh"),
    )
    .unwrap();

    assembly(&tmp)
        .args(["run", "graph.toml"])
        .assert()
        .success();

    assembly(&tmp)
        .args(["revise", "1", "n", "add error handling"])
        .assert()
        .success()
        // The node's own outcome, not the run's. Reprinting the run summary
        // here reads as a contradiction: it belongs to the run that already
        // finished, so it would say "ok" beside a freshly failed node.
        .stdout(contains("round 2").and(contains("done")))
        .stdout(contains("run 1:").not());

    // Two commits on the node's branch, not one replaced by another.
    let commits = std::process::Command::new("git")
        .args([
            "-C",
            tmp.path().to_str().unwrap(),
            "rev-list",
            "--count",
            "al/run-1-n",
        ])
        .output()
        .unwrap();
    let count: usize = String::from_utf8_lossy(&commits.stdout)
        .trim()
        .parse()
        .unwrap();
    assert!(
        count >= 3,
        "expected base + two rounds, got {count} commits"
    );

    // The agent appended, which it could only do having seen round 1's file.
    let file = std::process::Command::new("git")
        .args([
            "-C",
            tmp.path().to_str().unwrap(),
            "show",
            "al/run-1-n:rounds.txt",
        ])
        .output()
        .unwrap();
    let body = String::from_utf8_lossy(&file.stdout);
    assert!(
        body.starts_with("hi\n"),
        "round 1's line is gone, so round 2 started from scratch: {body}"
    );
    assert!(
        body.contains("add error handling"),
        "round 2 never saw the feedback: {body}"
    );

    discard_worktrees(&tmp);
}

/// Feedback is a required argument now that there is no inbox to have left it
/// in — clap refuses the invocation before assembly ever sees it.
#[test]
fn revise_without_feedback_is_a_usage_error() {
    let tmp = repo_with_commit();
    std::fs::write(tmp.path().join("graph.toml"), agent_graph("fake-agent.sh")).unwrap();

    assembly(&tmp)
        .args(["run", "graph.toml"])
        .assert()
        .success();

    assembly(&tmp).args(["revise", "1", "n"]).assert().code(2);

    discard_worktrees(&tmp);
}

#[test]
fn a_run_never_touches_the_target_repositorys_working_tree() {
    let tmp = repo_with_commit();
    std::fs::write(tmp.path().join("graph.toml"), agent_graph("fake-agent.sh")).unwrap();
    let at = tmp.path().to_str().unwrap();

    let before = std::process::Command::new("git")
        .args(["-C", at, "rev-parse", "HEAD"])
        .output()
        .unwrap()
        .stdout;

    assembly(&tmp)
        .args(["run", "graph.toml"])
        .assert()
        .success();

    let after = std::process::Command::new("git")
        .args(["-C", at, "rev-parse", "HEAD"])
        .output()
        .unwrap()
        .stdout;
    assert_eq!(before, after, "HEAD moved");

    // `.assembly/` and the graph are the only things in the tree, and both are
    // untracked — the agent's work went to a branch, not to this checkout.
    let dirty = std::process::Command::new("git")
        .args(["-C", at, "status", "--porcelain", "--untracked-files=no"])
        .output()
        .unwrap()
        .stdout;
    assert!(
        dirty.is_empty(),
        "tracked files changed: {}",
        String::from_utf8_lossy(&dirty)
    );
    assert!(!tmp.path().join("agent-output.txt").exists());

    discard_worktrees(&tmp);
}

#[test]
fn gc_older_than_collects_a_live_runs_worktrees_but_only_when_asked() {
    let tmp = repo_with_commit();
    std::fs::write(
        tmp.path().join("graph.toml"),
        agent_graph("failing-agent.sh"),
    )
    .unwrap();
    assembly(&tmp).args(["run", "graph.toml"]).assert().code(1);

    let worktrees = run_worktrees(&tmp, 1);
    assert!(worktrees.is_dir());

    // The run's state is still there, so nothing is stale by default...
    assembly(&tmp)
        .args(["gc"])
        .assert()
        .success()
        .stdout(contains("nothing to collect"));
    assert!(worktrees.exists());

    // ...but an explicit age cutoff sweeps it anyway.
    assembly(&tmp)
        .args(["gc", "--older-than", "0s"])
        .assert()
        .success()
        .stdout(contains("untouched for"));
    assert!(!worktrees.exists());

    discard_worktrees(&tmp);
}

#[test]
fn gc_rejects_an_unreadable_duration() {
    let tmp = repo();
    assembly(&tmp)
        .args(["gc", "--older-than", "soon"])
        .assert()
        .code(2)
        .stderr(contains("soon"));
}
