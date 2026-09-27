use assembly_line::config::REPO_CONFIG_PATH;
use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use support::commit_all;

mod support;

fn assembly(tmp: &tempfile::TempDir) -> Command {
    let mut cmd = Command::cargo_bin("assembly").unwrap();
    cmd.current_dir(tmp.path());
    cmd
}

fn git_in(dir: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(["-C", dir.to_str().unwrap()])
        .args(args)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn git(tmp: &tempfile::TempDir, args: &[&str]) -> String {
    git_in(tmp.path(), args)
}

/// Where this test's bare remote lives: outside the repository, so the
/// repository's own `git status` stays clean.
fn origin_for(tmp: &tempfile::TempDir) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "assembly-test-origin-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    ))
}

/// `git` run against this test's remote, which is where a job's branch lives.
fn git_on_origin(tmp: &tempfile::TempDir, args: &[&str]) -> String {
    git_in(&origin_for(tmp), args)
}

/// Give the repository a remote and publish `main` to it — every job clones
/// from one.
async fn publish_to_origin(tmp: &tempfile::TempDir) {
    support::add_origin(tmp.path(), &origin_for(tmp)).await;
    support::publish_main(tmp.path()).await;
}

/// A repository that has opted in: its `.assembly/config.toml` is committed,
/// which is the only thing that makes it runnable, and published to its
/// origin, which is where a job starts from.
///
/// The repository itself comes from `support`, so there is one definition of
/// "a git repo with a commit in it" across the whole test suite.
async fn repo_running(script: &str) -> tempfile::TempDir {
    repo_opted_in_with(&format!(
        "verify = \"true\"\n{}",
        support::config_running(script)
    ))
    .await
}

/// Like [`repo_running`], but the config also declares `copy` — which no
/// container runner can honour.
async fn repo_running_with_copy(script: &str) -> tempfile::TempDir {
    repo_opted_in_with(&format!(
        "verify = \"true\"\ncopy = [\"local.env\"]\n{}",
        support::config_running(script)
    ))
    .await
}

/// A repository whose committed, published `.assembly/config.toml` is `config`.
async fn repo_opted_in_with(config: &str) -> tempfile::TempDir {
    let tmp = support::repo_with_initial_commit().await;
    std::fs::create_dir_all(tmp.path().join(".assembly")).unwrap();
    std::fs::write(tmp.path().join(REPO_CONFIG_PATH), config).unwrap();
    commit_all(tmp.path(), "opt in").await.unwrap().unwrap();
    publish_to_origin(&tmp).await;
    tmp
}

/// The bare remote outlives the tempdir, so a test that makes one has to take
/// it with it.
fn discard_origin(tmp: &tempfile::TempDir) {
    let _ = std::fs::remove_dir_all(origin_for(tmp));
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

    discard_origin(&tmp);
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
        git_on_origin(&tmp, &["rev-parse", "--verify", "-q", "al/job-1"]).len(),
        40
    );

    discard_origin(&tmp);
}

/// End to end, for the rule `paths::job_id_past` owns.
#[tokio::test]
async fn a_job_id_already_taken_on_the_remote_is_skipped() {
    let tmp = repo_running("fake-agent.sh").await;
    // Pushed from elsewhere, and not a commit this job would fast-forward:
    // nothing about it exists under `.assembly/jobs`.
    let elsewhere = git(
        &tmp,
        &["commit-tree", "HEAD^{tree}", "-p", "HEAD", "-m", "theirs"],
    );
    git(
        &tmp,
        &[
            "push",
            "--quiet",
            "origin",
            &format!("{elsewhere}:refs/heads/al/job-1"),
        ],
    );
    let theirs = git_on_origin(&tmp, &["rev-parse", "al/job-1"]);

    assembly(&tmp)
        .args(["run", "--prompt", "x"])
        .assert()
        .success()
        .stdout(contains("job 2: succeeded"));

    assert_eq!(
        git_on_origin(&tmp, &["rev-parse", "al/job-1"]),
        theirs,
        "somebody else's job branch moved"
    );
    assert_eq!(
        git_on_origin(&tmp, &["rev-parse", "--verify", "-q", "al/job-2"]).len(),
        40
    );
    assert!(tmp.path().join(".assembly/jobs/2/events.jsonl").is_file());

    discard_origin(&tmp);
}

#[tokio::test]
async fn a_repository_that_has_not_opted_in_is_told_which_file_to_write() {
    let tmp = support::repo_with_initial_commit().await;
    publish_to_origin(&tmp).await;

    assembly(&tmp)
        .args(["run", "--prompt", "x"])
        .assert()
        .code(2)
        .stderr(contains(".assembly/config.toml"));

    assert!(
        !tmp.path().join(".assembly/jobs/1").exists(),
        "a repository that cannot run should not allocate a job directory"
    );

    discard_origin(&tmp);
}

/// A job clones from the remote and pushes back to it, so a repository
/// without one is told to add one — not to push to a remote it lacks.
#[tokio::test]
async fn a_repository_with_no_remote_is_told_to_add_one() {
    let tmp = support::repo_with_initial_commit().await;

    assembly(&tmp)
        .args(["run", "--prompt", "x"])
        .assert()
        .code(2)
        .stderr(contains("no 'origin' remote").and(contains("add one")))
        .stderr(contains("push it first").not());
}

/// A first round that committed nothing pushed nothing, so there is no
/// branch on the remote to continue.
#[tokio::test]
async fn revising_a_job_that_left_no_branch_says_there_is_nothing_to_revise() {
    let tmp = repo_running("noop-agent.sh").await;
    assembly(&tmp)
        .args(["run", "--prompt", "x"])
        .assert()
        .success();

    assembly(&tmp)
        .args(["revise", "1", "try again"])
        .assert()
        .code(2)
        .stderr(contains("job 1 has no branch on 'origin'").and(contains("nothing to revise")));

    discard_origin(&tmp);
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

    discard_origin(&tmp);
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

    discard_origin(&tmp);
}

#[tokio::test]
async fn a_prompt_can_come_from_a_file_instead() {
    let tmp = repo_running("fake-agent.sh").await;
    std::fs::write(tmp.path().join("auth.md"), "Implement auth").unwrap();

    assembly(&tmp)
        .args(["run", "--prompt-file", "auth.md"])
        .assert()
        .success();

    let on_branch = git_on_origin(&tmp, &["show", "al/job-1:agent-output.txt"]);
    assert!(on_branch.contains("Implement auth"), "{on_branch}");

    discard_origin(&tmp);
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

    discard_origin(&tmp);
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

    discard_origin(&tmp);
}

/// The token is for the container runners, which have no credentials of
/// their own. Exported for them, it must not take over a local run, which
/// uses whatever git already authenticates with on this machine.
#[tokio::test]
async fn the_local_runner_keeps_the_hosts_credentials_even_with_a_token_exported() {
    let tmp = repo_running("credential-reporting-agent.sh").await;

    assembly(&tmp)
        .env("ASSEMBLY_GIT_TOKEN", "t")
        .args(["run", "--prompt", "x"])
        .assert()
        .success();

    assembly(&tmp)
        .args(["logs", "1"])
        .assert()
        .success()
        .stdout(contains("helpers:"))
        .stdout(contains("x-access-token").not());

    discard_origin(&tmp);
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

    discard_origin(&target);
}

/// A global `pushInsteadOf` — fetch over one transport, push over another —
/// is the user's own config, which the job's clone reads too: the job runs,
/// and its branch goes where the user's own push would.
#[tokio::test]
async fn a_global_push_rewrite_is_followed_rather_than_refused() {
    let tmp = repo_running("fake-agent.sh").await;
    let elsewhere = tempfile::tempdir().unwrap();
    let pushed_to = elsewhere.path().join("pushed.git");
    git_in(
        elsewhere.path(),
        &["init", "--quiet", "--bare", pushed_to.to_str().unwrap()],
    );
    let global_config = elsewhere.path().join("gitconfig");
    std::fs::write(
        &global_config,
        format!(
            "[url \"{}\"]\n\tpushInsteadOf = {}\n",
            pushed_to.display(),
            origin_for(&tmp).display()
        ),
    )
    .unwrap();

    assembly(&tmp)
        .env("GIT_CONFIG_GLOBAL", &global_config)
        .args(["run", "--prompt", "x"])
        .assert()
        .success();
    assert_eq!(
        git_in(&pushed_to, &["rev-parse", "--verify", "-q", "al/job-1"]).len(),
        40,
        "the job's push did not follow the rewrite"
    );

    discard_origin(&tmp);
}

/// `--ref` decides what the job is cut from; with none, the checked-out
/// branch does.
#[tokio::test]
async fn a_job_branches_from_the_ref_it_is_given() {
    let tmp = repo_running("fake-agent.sh").await;
    let earlier = git(&tmp, &["rev-parse", "HEAD"]);
    git(&tmp, &["tag", "start-here"]);
    git(&tmp, &["push", "--quiet", "origin", "start-here"]);

    std::fs::write(tmp.path().join("later.txt"), "after\n").unwrap();
    commit_all(tmp.path(), "later work").await.unwrap().unwrap();
    support::publish_main(tmp.path()).await;
    assert_ne!(git(&tmp, &["rev-parse", "HEAD"]), earlier);

    assembly(&tmp)
        .args(["run", "--ref", "start-here", "--prompt", "x"])
        .assert()
        .success();
    assert_eq!(
        git_on_origin(&tmp, &["rev-parse", "al/job-1^"]),
        earlier,
        "--ref was ignored"
    );

    // With no --ref the job follows the checked-out branch instead.
    assembly(&tmp)
        .args(["run", "--prompt", "x"])
        .assert()
        .success();
    assert_eq!(
        git_on_origin(&tmp, &["rev-parse", "al/job-2^"]),
        git(&tmp, &["rev-parse", "main"]),
        "the default base ref is not the checked-out branch"
    );

    discard_origin(&tmp);
}

#[tokio::test]
async fn a_detached_head_is_asked_to_name_its_ref() {
    let tmp = repo_running("fake-agent.sh").await;
    git(&tmp, &["checkout", "--detach"]);

    assembly(&tmp)
        .args(["run", "--prompt", "x"])
        .assert()
        .code(2)
        .stderr(contains("--ref"));

    discard_origin(&tmp);
}

#[tokio::test]
async fn unpushed_local_work_is_pointed_out_and_the_remotes_ref_is_used() {
    let tmp = repo_running("fake-agent.sh").await;
    std::fs::write(tmp.path().join("unpushed.txt"), "mine\n").unwrap();
    commit_all(tmp.path(), "unpushed").await.unwrap().unwrap();

    assembly(&tmp)
        .args(["run", "--prompt", "x"])
        .assert()
        .success()
        .stdout(contains("push first"));

    discard_origin(&tmp);
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
    let count: usize = git_on_origin(&tmp, &["rev-list", "--count", "al/job-1"])
        .parse()
        .unwrap();
    // README, the opt-in commit, then one per round. Three would mean round 2
    // cut a fresh branch off the base instead of continuing round 1's.
    assert_eq!(count, 4, "expected base + two rounds");

    let body = git_on_origin(&tmp, &["show", "al/job-1:rounds.txt"]);
    assert!(
        body.starts_with("hi\n"),
        "round 1's line is gone, so round 2 started from scratch: {body}"
    );
    assert!(
        body.contains("add error handling"),
        "round 2 never saw the feedback: {body}"
    );

    discard_origin(&tmp);
}

/// Feedback is a required argument — clap refuses the invocation before
/// assembly ever sees it.
#[tokio::test]
async fn revise_without_feedback_is_a_usage_error() {
    let tmp = repo_running("fake-agent.sh").await;

    assembly(&tmp).args(["revise", "1"]).assert().code(2);

    discard_origin(&tmp);
}

#[tokio::test]
async fn a_job_writes_nothing_outside_dot_assembly_in_the_target_repository() {
    let tmp = repo_running("fake-agent.sh").await;
    let before = git(&tmp, &["rev-parse", "HEAD"]);

    assembly(&tmp)
        .args(["run", "--prompt", "x"])
        .assert()
        .success();

    assert_eq!(before, git(&tmp, &["rev-parse", "HEAD"]), "HEAD moved");

    // Full porcelain, untracked files included: job state legitimately lands
    // under `.assembly/` (until a later milestone moves it out of the repo
    // entirely), but nothing else in the working tree may change — tracked
    // or not. `--untracked-files=no` would blind this to exactly the files
    // the binary just wrote.
    let status = git(&tmp, &["status", "--porcelain"]);
    let stray: Vec<&str> = status
        .lines()
        .filter(|line| !line[3..].starts_with(".assembly/"))
        .collect();
    assert!(stray.is_empty(), "wrote outside .assembly/: {stray:?}");

    assert!(!tmp.path().join("agent-output.txt").exists());

    discard_origin(&tmp);
}

#[tokio::test]
async fn container_flags_are_refused_for_the_local_runner() {
    let tmp = repo_running("fake-agent.sh").await;
    assembly(&tmp)
        .args(["run", "--prompt", "x", "--pass-env", "ANTHROPIC_API_KEY"])
        .assert()
        .code(2)
        .stderr(contains("container runners"));
    discard_origin(&tmp);
}

/// The docker runner through the real binary, with a fake `docker` first on
/// PATH. Every preflight problem is reported at once, and nothing is
/// allocated.
#[tokio::test]
async fn docker_preflight_reports_every_problem_before_allocating() {
    let tmp = repo_running_with_copy("fake-agent.sh").await;
    let fakes = tempfile::tempdir().unwrap();
    // A docker that cannot reach its daemon.
    support::fake_cli(fakes.path(), "docker", "echo 'no daemon' >&2\nexit 1\n");

    assembly(&tmp)
        .env(
            "PATH",
            format!(
                "{}:{}",
                fakes.path().display(),
                std::env::var("PATH").unwrap()
            ),
        )
        .env_remove("ASSEMBLY_GIT_TOKEN")
        .args(["run", "--prompt", "x", "--runner", "docker"])
        .assert()
        .code(2)
        .stderr(contains("`docker` cannot be reached"))
        .stderr(contains("declares `copy`"))
        .stderr(contains("$ASSEMBLY_GIT_TOKEN is not set"))
        .stderr(contains("is a path on this machine"));

    assert!(!tmp.path().join(".assembly/jobs/1").exists());
    discard_origin(&tmp);
}

/// The test origin is a directory on this machine: fine for the local
/// runner, but no container can clone it. The docker runner says so before
/// anything is allocated, even with a daemon and a token to hand — and
/// refuses a `--pass-env` that would override the payload in the same
/// breath.
#[tokio::test]
async fn a_remote_that_is_a_local_path_is_refused_for_a_container_runner() {
    let tmp = repo_running("fake-agent.sh").await;
    let fakes = tempfile::tempdir().unwrap();
    // A docker whose daemon answers.
    support::fake_cli(fakes.path(), "docker", "echo 27.0.0\n");

    assembly(&tmp)
        .env(
            "PATH",
            format!(
                "{}:{}",
                fakes.path().display(),
                std::env::var("PATH").unwrap()
            ),
        )
        .env("ASSEMBLY_GIT_TOKEN", "t0ken")
        .args([
            "run",
            "--prompt",
            "x",
            "--runner",
            "docker",
            "--pass-env",
            "ASSEMBLY_JOB",
        ])
        .assert()
        .code(2)
        .stderr(contains("is a path on this machine").and(contains("--runner local")))
        .stderr(contains("--pass-env ASSEMBLY_JOB").and(contains("drop it")));

    assert!(!tmp.path().join(".assembly/jobs/1").exists());
    discard_origin(&tmp);
}

#[tokio::test]
async fn the_k8s_runner_requires_a_namespace() {
    let tmp = repo_running("fake-agent.sh").await;
    assembly(&tmp)
        .args(["run", "--prompt", "x", "--runner", "k8s"])
        .assert()
        .code(2)
        .stderr(contains("--namespace"));
    discard_origin(&tmp);
}

#[tokio::test]
async fn a_namespace_is_refused_for_runners_that_have_none() {
    let tmp = repo_running("fake-agent.sh").await;
    assembly(&tmp)
        .args([
            "run",
            "--prompt",
            "x",
            "--runner",
            "docker",
            "--namespace",
            "factory",
        ])
        .assert()
        .code(2)
        .stderr(contains("k8s runner"));
    discard_origin(&tmp);
}
