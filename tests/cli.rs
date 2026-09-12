use assembly_line::config::REPO_CONFIG_PATH;
use assembly_line::git::commit_all;
use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;

mod support;

/// Where this test's worktrees go: outside the repository, and outside the
/// shared `$HOME` default. `gc` walks every repository it can see, so tests
/// sharing one root would collect each other's work mid-job.
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

fn git(tmp: &tempfile::TempDir, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(["-C", tmp.path().to_str().unwrap()])
        .args(args)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A repository that has opted in: its `.assembly/config.toml` is committed,
/// which is the only thing that makes it runnable.
///
/// The repository itself comes from `support`, so there is one definition of
/// "a git repo with a commit in it" across the whole test suite.
async fn repo_running(script: &str) -> tempfile::TempDir {
    let tmp = support::repo_with_initial_commit().await;
    std::fs::create_dir_all(tmp.path().join(".assembly")).unwrap();
    std::fs::write(
        tmp.path().join(REPO_CONFIG_PATH),
        // `verify` only silences the warning; Task 6 gives it teeth.
        format!("verify = \"true\"\n{}", support::config_running(script)),
    )
    .unwrap();
    commit_all(tmp.path(), "opt in").await.unwrap().unwrap();
    tmp
}

/// Where the binary will put job `id`'s worktrees, given the root this test
/// hands it. On macOS a tempdir sits under a symlink, so the slug must be
/// computed from the path the binary itself resolves.
fn job_worktrees(tmp: &tempfile::TempDir, id: u64) -> std::path::PathBuf {
    worktree_root_for(tmp)
        .join(assembly_line::paths::repo_slug(
            &std::fs::canonicalize(tmp.path()).unwrap(),
        ))
        .join(id.to_string())
}

/// Worktrees outlive the tempdir, so a test that starts a job has to take them
/// with it.
fn discard_worktrees(tmp: &tempfile::TempDir) {
    let _ = std::fs::remove_dir_all(worktree_root_for(tmp));
}

#[tokio::test]
async fn run_exits_zero_and_records_the_job() {
    let tmp = repo_running("fake-agent.sh").await;

    assembly(&tmp)
        .args(["run", "--prompt", "do the thing"])
        .assert()
        .success()
        .stdout(contains("job 1: succeeded"));

    assert!(tmp.path().join(".assembly/jobs/1/events.jsonl").is_file());
    assert!(tmp.path().join(".assembly/jobs/1/meta.json").is_file());

    discard_worktrees(&tmp);
}

#[tokio::test]
async fn run_exits_one_when_the_agent_fails_but_still_leaves_the_branch() {
    let tmp = repo_running("failing-agent.sh").await;

    assembly(&tmp)
        .args(["run", "--prompt", "x"])
        .assert()
        .code(1)
        .stdout(contains("job 1: failed").and(contains("exit 3")));

    // The branch is the durable artifact, and it survives a failure.
    assert_eq!(
        git(&tmp, &["rev-parse", "--verify", "-q", "al/job-1"]).len(),
        40
    );

    discard_worktrees(&tmp);
}

#[tokio::test]
async fn a_repository_that_has_not_opted_in_is_told_which_file_to_write() {
    let tmp = support::repo_with_initial_commit().await;

    assembly(&tmp)
        .args(["run", "--prompt", "x"])
        .assert()
        .code(2)
        .stderr(contains(".assembly/config.toml"));

    assert!(
        !tmp.path().join(".assembly/jobs/1").exists(),
        "a repository that cannot run should not allocate a job directory"
    );
}

#[tokio::test]
async fn an_undeclared_provider_is_rejected_before_a_job_directory_is_allocated() {
    let tmp = repo_running("fake-agent.sh").await;

    assembly(&tmp)
        .args(["run", "--prompt", "x", "--provider", "ghost"])
        .assert()
        .code(2)
        .stderr(contains("ghost").and(contains("add a block for it")));

    assert!(!tmp.path().join(".assembly/jobs/1").exists());
}

#[tokio::test]
async fn run_outside_a_git_repo_explains_itself() {
    let tmp = tempfile::tempdir().unwrap();

    Command::cargo_bin("assembly")
        .unwrap()
        .current_dir(tmp.path())
        .args(["run", "--prompt", "x"])
        .assert()
        .code(2)
        .stderr(contains("not inside a git repository"));
}

#[tokio::test]
async fn a_run_with_no_prompt_at_all_is_a_usage_error() {
    let tmp = repo_running("fake-agent.sh").await;

    assembly(&tmp).arg("run").assert().code(2);
}

#[tokio::test]
async fn a_prompt_can_come_from_a_file_instead() {
    let tmp = repo_running("fake-agent.sh").await;
    std::fs::write(tmp.path().join("auth.md"), "Implement auth").unwrap();

    assembly(&tmp)
        .args(["run", "--prompt-file", "auth.md"])
        .assert()
        .success();

    let on_branch = git(&tmp, &["show", "al/job-1:agent-output.txt"]);
    assert!(on_branch.contains("Implement auth"), "{on_branch}");

    discard_worktrees(&tmp);
}

#[tokio::test]
async fn a_missing_prompt_file_is_reported_before_the_job_starts() {
    let tmp = repo_running("fake-agent.sh").await;

    assembly(&tmp)
        .args(["run", "--prompt-file", "gone.md"])
        .assert()
        .code(2)
        .stderr(contains("gone.md"));

    assert!(!tmp.path().join(".assembly/jobs/1").exists());
}

#[tokio::test]
async fn status_and_logs_report_a_finished_job() {
    let tmp = repo_running("failing-agent.sh").await;

    assembly(&tmp)
        .args(["run", "--prompt", "x"])
        .assert()
        .code(1);

    // No job id given — defaults to the most recent job.
    assembly(&tmp)
        .arg("status")
        .assert()
        .success()
        .stdout(contains("job 1: failed"))
        .stdout(contains("exit 3"))
        .stdout(contains("took"))
        .stdout(contains("branch: al/job-1"));

    assembly(&tmp)
        .args(["logs", "1"])
        .assert()
        .success()
        .stdout(contains("giving up"));

    discard_worktrees(&tmp);
}

/// Every command resolves the repository the same way, so a job started with
/// `--repo` is findable by `status`, `logs` and `revise` with the same
/// `--repo` — and invisible without it.
#[tokio::test]
async fn a_job_started_elsewhere_is_found_by_pointing_the_read_commands_at_it() {
    let target = repo_running("fake-agent.sh").await;
    let standing_in = support::repo_with_initial_commit().await;
    let at = target.path().to_str().unwrap().to_string();

    assembly(&standing_in)
        .args(["run", "--repo", &at, "--prompt", "x"])
        .assert()
        .success();

    // The state went to the repository named, not to the one we stood in.
    assert!(
        target
            .path()
            .join(".assembly/jobs/1/events.jsonl")
            .is_file()
    );
    assert!(!standing_in.path().join(".assembly").exists());

    // Without --repo the job is invisible from here...
    assembly(&standing_in)
        .arg("status")
        .assert()
        .code(2)
        .stderr(contains("no jobs yet"));

    // ...and found with it.
    assembly(&standing_in)
        .args(["status", "--repo", &at])
        .assert()
        .success()
        .stdout(contains("job 1: succeeded"));

    assembly(&standing_in)
        .args(["logs", "1", "--repo", &at])
        .assert()
        .success()
        .stdout(contains("fake-agent"));

    assembly(&standing_in)
        .args(["revise", "1", "do it again", "--repo", &at])
        .assert()
        .success()
        .stdout(contains("round 2"));

    discard_worktrees(&standing_in);
}

/// `--ref` decides what the job is cut from; with none, the checked-out
/// branch does.
#[tokio::test]
async fn a_job_branches_from_the_ref_it_is_given() {
    let tmp = repo_running("fake-agent.sh").await;
    let earlier = git(&tmp, &["rev-parse", "HEAD"]);
    git(&tmp, &["tag", "start-here"]);

    std::fs::write(tmp.path().join("later.txt"), "after\n").unwrap();
    commit_all(tmp.path(), "later work").await.unwrap().unwrap();
    assert_ne!(git(&tmp, &["rev-parse", "HEAD"]), earlier);

    assembly(&tmp)
        .args(["run", "--ref", "start-here", "--prompt", "x"])
        .assert()
        .success();
    assert_eq!(
        git(&tmp, &["rev-parse", "al/job-1^"]),
        earlier,
        "--ref was ignored"
    );

    // With no --ref the job follows the checked-out branch instead.
    assembly(&tmp)
        .args(["run", "--prompt", "x"])
        .assert()
        .success();
    assert_eq!(
        git(&tmp, &["rev-parse", "al/job-2^"]),
        git(&tmp, &["rev-parse", "main"]),
        "the default base ref is not the checked-out branch"
    );

    discard_worktrees(&tmp);
}

#[tokio::test]
async fn status_with_no_jobs_explains_itself() {
    let tmp = support::repo_with_initial_commit().await;
    assembly(&tmp)
        .arg("status")
        .assert()
        .code(2)
        .stderr(contains("no jobs yet"));
}

#[tokio::test]
async fn logs_for_an_unknown_job_explains_itself() {
    let tmp = support::repo_with_initial_commit().await;
    assembly(&tmp)
        .args(["logs", "42"])
        .assert()
        .code(2)
        .stderr(contains("no such job: 42"));
}

/// The heart of the stateless design: a revise is a new job, and the agent
/// sees its prior work because that work *is* the branch it starts from.
/// Nothing was kept on disk between the two rounds.
#[tokio::test]
async fn a_revise_round_continues_the_branch_instead_of_starting_over() {
    let tmp = repo_running("revising-agent.sh").await;

    assembly(&tmp)
        .args(["run", "--prompt", "hi"])
        .assert()
        .success();

    assembly(&tmp)
        .args(["revise", "1", "add error handling"])
        .assert()
        .success()
        .stdout(contains("round 2").and(contains("job 1: succeeded")));

    // Two commits on the job's branch, not one replaced by another.
    let count: usize = git(&tmp, &["rev-list", "--count", "al/job-1"])
        .parse()
        .unwrap();
    assert!(
        count >= 3,
        "expected base + two rounds, got {count} commits"
    );

    let body = git(&tmp, &["show", "al/job-1:rounds.txt"]);
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

/// Feedback is a required argument — clap refuses the invocation before
/// assembly ever sees it.
#[tokio::test]
async fn revise_without_feedback_is_a_usage_error() {
    let tmp = repo_running("fake-agent.sh").await;

    assembly(&tmp).args(["revise", "1"]).assert().code(2);
}

#[tokio::test]
async fn a_job_never_touches_the_target_repositorys_working_tree() {
    let tmp = repo_running("fake-agent.sh").await;
    let before = git(&tmp, &["rev-parse", "HEAD"]);

    assembly(&tmp)
        .args(["run", "--prompt", "x"])
        .assert()
        .success();

    assert_eq!(before, git(&tmp, &["rev-parse", "HEAD"]), "HEAD moved");
    assert!(
        git(&tmp, &["status", "--porcelain", "--untracked-files=no"]).is_empty(),
        "tracked files changed"
    );
    assert!(!tmp.path().join("agent-output.txt").exists());

    discard_worktrees(&tmp);
}

#[tokio::test]
async fn gc_with_nothing_to_collect_says_so() {
    let tmp = repo_running("fake-agent.sh").await;
    assembly(&tmp)
        .args(["run", "--prompt", "x"])
        .assert()
        .success();

    assembly(&tmp)
        .args(["gc", "--dry-run"])
        .assert()
        .success()
        .stdout(contains("nothing to collect"));

    discard_worktrees(&tmp);
}

#[tokio::test]
async fn gc_collects_a_jobs_worktrees_only_once_its_job_state_is_gone() {
    let tmp = repo_running("failing-agent.sh").await;
    assembly(&tmp)
        .args(["run", "--prompt", "x"])
        .assert()
        .code(1);

    let worktrees = job_worktrees(&tmp, 1);
    assert!(
        !worktrees.join("checkout").exists(),
        "a job's checkout is scratch — even a failed one discards it"
    );

    // The job still exists, so its worktrees are still wanted.
    assembly(&tmp)
        .args(["gc"])
        .assert()
        .success()
        .stdout(contains("nothing to collect"));
    assert!(worktrees.exists());

    std::fs::remove_dir_all(tmp.path().join(".assembly/jobs")).unwrap();

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

#[tokio::test]
async fn gc_older_than_collects_a_live_jobs_worktrees_but_only_when_asked() {
    let tmp = repo_running("failing-agent.sh").await;
    assembly(&tmp)
        .args(["run", "--prompt", "x"])
        .assert()
        .code(1);

    let worktrees = job_worktrees(&tmp, 1);
    assert!(worktrees.is_dir());

    // The job's state is still there, so nothing is stale by default...
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

#[tokio::test]
async fn gc_rejects_an_unreadable_duration() {
    let tmp = support::repo_with_initial_commit().await;
    assembly(&tmp)
        .args(["gc", "--older-than", "soon"])
        .assert()
        .code(2)
        .stderr(contains("soon"));
}
