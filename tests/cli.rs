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
    let tmp = with_graph("[[task]]\nid=\"a\"\nkind=\"shell\"\nrun=\"true\"\n");
    assembly(&tmp)
        .args(["validate", "graph.toml"])
        .assert()
        .success()
        .stdout(contains("ok (1 tasks)"));
}

#[test]
fn validate_reports_a_cycle_and_exits_two() {
    let tmp = with_graph(
        "[[task]]\nid=\"a\"\nkind=\"shell\"\nneeds=[\"b\"]\nrun=\"true\"\n\
         [[task]]\nid=\"b\"\nkind=\"shell\"\nneeds=[\"a\"]\nrun=\"true\"\n",
    );
    assembly(&tmp)
        .args(["validate", "graph.toml"])
        .assert()
        .code(2)
        .stderr(contains("dependency cycle"));
}

#[test]
fn validate_warns_about_an_unsupervised_agent_without_verify() {
    let tmp = with_graph(
        "[providers.p]\ncmd=\"true\"\n\
         [[task]]\nid=\"a\"\nkind=\"agent\"\nprompt=\"hi\"\nprovider=\"p\"\n",
    );
    assembly(&tmp)
        .args(["validate", "graph.toml"])
        .assert()
        .success()
        .stderr(contains("nothing will check its output"));
}

#[test]
fn run_exits_zero_and_records_the_run() {
    let tmp = with_graph("[[task]]\nid=\"a\"\nkind=\"shell\"\nrun=\"true\"\n");
    assembly(&tmp)
        .args(["run", "graph.toml"])
        .assert()
        .success()
        .stdout(contains("run 1").and(contains("ok")));

    assert!(tmp.path().join(".assembly/runs/1/events.jsonl").is_file());
    assert!(tmp.path().join(".assembly/runs/1/meta.json").is_file());
}

#[test]
fn run_exits_one_when_a_node_fails() {
    let tmp = with_graph("[[task]]\nid=\"a\"\nkind=\"shell\"\nrun=\"exit 1\"\n");
    assembly(&tmp)
        .args(["run", "graph.toml"])
        .assert()
        .code(1)
        .stdout(contains("partial"));
}

#[test]
fn run_exits_two_on_an_invalid_graph_and_leaves_no_run_directory() {
    let tmp = with_graph("[[task]]\nid=\"a\"\nkind=\"shell\"\nneeds=[\"ghost\"]\nrun=\"true\"\n");
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
        "[[task]]\nid=\"a\"\nkind=\"shell\"\nrun=\"true\"\n",
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
    let tmp = with_graph(
        "[[task]]\nid=\"build\"\nkind=\"shell\"\nrun=\"echo building\"\n\
         [[task]]\nid=\"broken\"\nkind=\"shell\"\nneeds=[\"build\"]\nrun=\"echo nope 1>&2; exit 1\"\n\
         [[task]]\nid=\"after\"\nkind=\"shell\"\nneeds=[\"broken\"]\nrun=\"true\"\n",
    );

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
}

#[test]
fn logs_for_an_unknown_node_explains_itself() {
    let tmp = with_graph("[[task]]\nid=\"a\"\nkind=\"shell\"\nrun=\"true\"\n");
    assembly(&tmp)
        .args(["run", "graph.toml"])
        .assert()
        .success();

    assembly(&tmp)
        .args(["logs", "1", "ghost"])
        .assert()
        .code(2)
        .stderr(contains("no log for node 'ghost'"));
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
         [[task]]\nid=\"a\"\nkind=\"agent\"\nprovider=\"p\"\nprompt_file=\"auth.md\"\nverify=\"true\"\n",
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
         [[task]]\nid=\"a\"\nkind=\"agent\"\nprovider=\"p\"\nprompt_file=\"gone.md\"\nverify=\"true\"\n",
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
         [[task]]\nid = \"n\"\nkind = \"agent\"\nprovider = \"fake\"\nprompt = \"hi\"\n",
        path.display()
    )
}

#[test]
fn gc_with_nothing_to_collect_says_so() {
    let tmp = with_graph("[[task]]\nid=\"a\"\nkind=\"shell\"\nrun=\"true\"\n");
    assembly(&tmp)
        .args(["run", "graph.toml"])
        .assert()
        .success();

    assembly(&tmp)
        .args(["gc", "--dry-run"])
        .assert()
        .success()
        .stdout(contains("nothing to collect"));
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
    // What is left is the run's own integration checkout, which lives as long
    // as the run does.
    assert!(
        worktrees
            .join(assembly_line::paths::INTEGRATION_WORKTREE)
            .is_dir(),
        "the integration worktree is the run's, not a node's"
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

/// A graph whose single agent node asks for a gate. With nobody at the gate
/// the node still merges, and waits in the inbox instead of stalling the run.
fn gated_agent_graph(script: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(script);
    format!(
        "[providers.fake]\ncmd = \"bash\"\nargs = [\"{}\", \"{{prompt}}\", \"x\"]\n\
         [[task]]\nid = \"n\"\nkind = \"agent\"\nprovider = \"fake\"\nprompt = \"hi\"\n\
         supervise = \"on-complete\"\n",
        path.display()
    )
}

fn run_gated_agent() -> tempfile::TempDir {
    let tmp = repo_with_commit();
    std::fs::write(
        tmp.path().join("graph.toml"),
        gated_agent_graph("fake-agent.sh"),
    )
    .unwrap();
    assembly(&tmp)
        .args(["run", "graph.toml"])
        .assert()
        .success();
    tmp
}

#[test]
fn review_lists_a_deferred_gate_with_its_branch() {
    let tmp = run_gated_agent();

    assembly(&tmp).args(["review"]).assert().success().stdout(
        contains("1 awaiting review")
            .and(contains("n"))
            .and(contains("al/run-1-n")),
    );

    discard_worktrees(&tmp);
}

#[test]
fn approving_clears_the_node_from_the_inbox() {
    let tmp = run_gated_agent();

    assembly(&tmp)
        .args(["review", "--approve", "n"])
        .assert()
        .success()
        .stdout(contains("approved 'n'"));

    assembly(&tmp)
        .args(["review"])
        .assert()
        .success()
        .stdout(contains("nothing awaiting review"));

    discard_worktrees(&tmp);
}

#[test]
fn requesting_a_revision_records_the_feedback_and_points_at_the_next_step() {
    let tmp = run_gated_agent();

    assembly(&tmp)
        .args(["review", "--revise", "n", "use argon2, not bcrypt"])
        .assert()
        .success()
        .stdout(contains("sent 'n' back"));

    let log = std::fs::read_to_string(tmp.path().join(".assembly/runs/1/events.jsonl")).unwrap();
    assert!(log.contains("use argon2, not bcrypt"), "{log}");

    discard_worktrees(&tmp);
}

/// A verdict is only meaningful on work that actually reached a gate.
#[test]
fn ruling_on_a_node_that_is_not_awaiting_review_is_refused() {
    let tmp = run_gated_agent();

    assembly(&tmp)
        .args(["review", "--approve", "n"])
        .assert()
        .success();
    assembly(&tmp)
        .args(["review", "--approve", "n"])
        .assert()
        .failure()
        .stderr(contains("approved").and(contains("awaiting review")));

    assembly(&tmp)
        .args(["review", "--approve", "ghost"])
        .assert()
        .failure()
        .stderr(contains("no node 'ghost'"));

    discard_worktrees(&tmp);
}

#[test]
fn approve_and_revise_cannot_both_be_given() {
    let tmp = run_gated_agent();

    assembly(&tmp)
        .args(["review", "--approve", "n", "--revise", "n", "why"])
        .assert()
        .failure();

    discard_worktrees(&tmp);
}

#[test]
fn meta_records_the_run_branch_only_when_the_graph_has_agent_nodes() {
    let tmp = repo_with_commit();
    std::fs::write(tmp.path().join("graph.toml"), agent_graph("fake-agent.sh")).unwrap();

    assembly(&tmp)
        .args(["run", "graph.toml"])
        .assert()
        .success();
    let meta = std::fs::read_to_string(tmp.path().join(".assembly/runs/1/meta.json")).unwrap();
    assert!(meta.contains("al/run-1"), "{meta}");

    // A shell-only run creates no branch, so records none.
    std::fs::write(
        tmp.path().join("shell.toml"),
        "[[task]]\nid=\"a\"\nkind=\"shell\"\nrun=\"true\"\n",
    )
    .unwrap();
    assembly(&tmp)
        .args(["run", "shell.toml"])
        .assert()
        .success();
    let meta2 = std::fs::read_to_string(tmp.path().join(".assembly/runs/2/meta.json")).unwrap();
    assert!(!meta2.contains("al/run-2"), "{meta2}");

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
