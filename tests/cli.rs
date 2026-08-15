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

fn assembly(tmp: &tempfile::TempDir) -> Command {
    let mut cmd = Command::cargo_bin("assembly").unwrap();
    cmd.current_dir(tmp.path());
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
