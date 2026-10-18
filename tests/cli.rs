use assembly_line::cli::{Cli, Command as Subcommand, InapplicableFlags};
use assembly_line::config::REPO_CONFIG_PATH;
use assert_cmd::Command;
use clap::Parser;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use support::commit_all;

mod support;

fn assembly(tmp: &tempfile::TempDir) -> Command {
    let mut cmd = Command::cargo_bin("assembly").unwrap();
    cmd.current_dir(tmp.path())
        .env("PATH", support::path_where_gh_refuses())
        .env("ASSEMBLY_ROOT", root_for(tmp));
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

/// Where this test's state root lives: outside the repository, which a job
/// must leave untouched.
fn root_for(tmp: &tempfile::TempDir) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "assembly-test-root-{}",
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

/// A repository whose committed, published `.assembly/config.toml` is `config`.
async fn repo_opted_in_with(config: &str) -> tempfile::TempDir {
    let tmp = support::repo_with_initial_commit().await;
    std::fs::create_dir_all(tmp.path().join(".assembly")).unwrap();
    std::fs::write(tmp.path().join(REPO_CONFIG_PATH), config).unwrap();
    commit_all(tmp.path(), "opt in").await.unwrap().unwrap();
    publish_to_origin(&tmp).await;
    tmp
}

/// The bare remote and the state root outlive the tempdir, so a test that
/// makes them has to take them with it.
fn discard_outside_state(tmp: &tempfile::TempDir) {
    let _ = std::fs::remove_dir_all(origin_for(tmp));
    let _ = std::fs::remove_dir_all(root_for(tmp));
}

/// The job branches the remote carries, which is what a claim leaves.
fn job_branches_on_origin(tmp: &tempfile::TempDir) -> String {
    git_on_origin(tmp, &["branch", "--list", "al/job-*"])
}

#[tokio::test]
async fn run_exits_zero_and_leaves_the_jobs_branch() {
    let tmp = repo_running("fake-agent.sh").await;

    assembly(&tmp)
        .args(["run", "--prompt", "do the thing"])
        .assert()
        .success()
        .stdout(contains("job 1: passed"));

    assert_eq!(
        git_on_origin(&tmp, &["rev-parse", "--verify", "-q", "al/job-1"]).len(),
        40
    );

    discard_outside_state(&tmp);
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

    discard_outside_state(&tmp);
}

/// End to end, for the rule `claim::claim_job` owns.
#[tokio::test]
async fn a_job_id_already_taken_on_the_remote_is_skipped() {
    let tmp = repo_running("fake-agent.sh").await;
    // Pushed from elsewhere, and not a commit this job would fast-forward.
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
        .stdout(contains("job 2: passed"));

    assert_eq!(
        git_on_origin(&tmp, &["rev-parse", "al/job-1"]),
        theirs,
        "somebody else's job branch moved"
    );
    assert_eq!(
        git_on_origin(&tmp, &["rev-parse", "--verify", "-q", "al/job-2"]).len(),
        40
    );

    discard_outside_state(&tmp);
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

    assert_eq!(
        job_branches_on_origin(&tmp),
        "",
        "a repository that cannot run should not claim a job"
    );

    discard_outside_state(&tmp);
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

/// A job's branch is claimed before its round, so even a round that changed
/// nothing leaves one to revise.
#[tokio::test]
async fn a_job_whose_round_changed_nothing_can_still_be_revised() {
    let tmp = repo_running("noop-agent.sh").await;
    assembly(&tmp)
        .args(["run", "--prompt", "x"])
        .assert()
        .success();
    assert_eq!(
        git_on_origin(&tmp, &["rev-parse", "al/job-1"]),
        git(&tmp, &["rev-parse", "main"]),
        "the claimed branch should sit at the base"
    );

    assembly(&tmp)
        .args(["run", "--job", "1", "--prompt", "try again"])
        .assert()
        .success();

    discard_outside_state(&tmp);
}

#[tokio::test]
async fn an_undeclared_provider_is_rejected_before_anything_is_claimed() {
    let tmp = repo_running("fake-agent.sh").await;

    assembly(&tmp)
        .args(["run", "--prompt", "x", "--provider", "ghost"])
        .assert()
        .code(2)
        .stderr(contains("ghost").and(contains("add a block for it")));

    assert_eq!(job_branches_on_origin(&tmp), "");

    discard_outside_state(&tmp);
}

#[tokio::test]
async fn run_outside_a_git_repo_explains_itself() {
    let tmp = tempfile::tempdir().unwrap();

    Command::cargo_bin("assembly")
        .unwrap()
        .current_dir(tmp.path())
        .env("ASSEMBLY_ROOT", tmp.path().join("root"))
        .args(["run", "--prompt", "x"])
        .assert()
        .code(2)
        .stderr(contains("not inside a git repository"));
}

#[tokio::test]
async fn a_run_with_no_prompt_at_all_is_a_usage_error() {
    let tmp = repo_running("fake-agent.sh").await;

    assembly(&tmp).arg("run").assert().code(2);

    discard_outside_state(&tmp);
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

    discard_outside_state(&tmp);
}

#[tokio::test]
async fn a_missing_prompt_file_is_reported_before_the_job_starts() {
    let tmp = repo_running("fake-agent.sh").await;

    assembly(&tmp)
        .args(["run", "--prompt-file", "gone.md"])
        .assert()
        .code(2)
        .stderr(contains("gone.md"));

    assert_eq!(job_branches_on_origin(&tmp), "");

    discard_outside_state(&tmp);
}

/// A global `pushInsteadOf` — fetch over one transport, push over another —
/// is the user's own config, which the job's clone reads too: the job runs,
/// and its branch goes where the user's own push would.
///
/// The two URLs name one repository, as two transports do: what is pushed
/// to the one is mirrored into the other, where the round's `run` fetches
/// the job's claimed branch from.
#[tokio::test]
async fn a_global_push_rewrite_is_followed_rather_than_refused() {
    let tmp = repo_running("fake-agent.sh").await;
    let elsewhere = tempfile::tempdir().unwrap();
    let pushed_to = elsewhere.path().join("pushed.git");
    git_in(
        elsewhere.path(),
        &["init", "--quiet", "--bare", pushed_to.to_str().unwrap()],
    );
    support::fake_cli(
        &pushed_to.join("hooks"),
        "post-receive",
        &format!(
            // The mirror pushes without the rewrite, which would send it
            // straight back here.
            "while read -r _ new ref; do\n  \
               GIT_CONFIG_GLOBAL=/dev/null git push --quiet '{}' \"$new:$ref\"\n\
             done\n",
            origin_for(&tmp).display()
        ),
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

    discard_outside_state(&tmp);
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

    discard_outside_state(&tmp);
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

    discard_outside_state(&tmp);
}

#[tokio::test]
async fn status_with_no_jobs_explains_itself() {
    let tmp = support::repo_with_initial_commit().await;
    publish_to_origin(&tmp).await;

    assembly(&tmp)
        .arg("status")
        .assert()
        .code(2)
        .stderr(contains("no jobs yet"));

    discard_outside_state(&tmp);
}

#[tokio::test]
async fn logs_for_an_unknown_job_explains_itself() {
    let tmp = support::repo_with_initial_commit().await;
    publish_to_origin(&tmp).await;

    assembly(&tmp)
        .args(["logs", "42"])
        .assert()
        .code(2)
        .stderr(contains("no such job: 42"));

    discard_outside_state(&tmp);
}

/// The heart of the stateless design: a revise is a new round, and the agent
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
        .args(["run", "--job", "1", "--prompt", "add error handling"])
        .assert()
        .success()
        .stdout(contains("continuing al/job-1").and(contains("job 1: passed")));

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

    discard_outside_state(&tmp);
}

#[tokio::test]
async fn a_revise_can_take_its_prompt_from_a_file() {
    let tmp = repo_running("revising-agent.sh").await;
    std::fs::write(tmp.path().join("feedback.md"), "add error handling").unwrap();
    assembly(&tmp)
        .args(["run", "--prompt", "hi"])
        .assert()
        .success();

    assembly(&tmp)
        .args(["run", "--job", "1", "--prompt-file", "feedback.md"])
        .assert()
        .success();

    let body = git_on_origin(&tmp, &["show", "al/job-1:rounds.txt"]);
    assert!(body.contains("add error handling"), "{body}");

    discard_outside_state(&tmp);
}

/// A revise needs to be told what to change, like any submit.
#[tokio::test]
async fn a_revise_without_a_prompt_is_a_usage_error() {
    let tmp = repo_running("fake-agent.sh").await;

    assembly(&tmp)
        .args(["submit", "--job", "1"])
        .assert()
        .code(2)
        .stderr(contains("required arguments were not provided"));

    discard_outside_state(&tmp);
}

/// A revise starts from the job's own base and provider; naming others
/// would say something the revise cannot honour.
#[test]
fn a_revise_cannot_name_a_ref_or_a_provider() {
    for flag in ["--ref", "--provider"] {
        let parsed = Cli::try_parse_from([
            "assembly", "submit", "--job", "1", "--prompt", "fb", flag, "x",
        ]);
        assert!(parsed.is_err(), "{flag} was accepted alongside --job");
    }
}

#[tokio::test]
async fn a_job_writes_nothing_to_the_target_repositorys_working_tree() {
    let tmp = repo_running("fake-agent.sh").await;
    let before = git(&tmp, &["rev-parse", "HEAD"]);

    assembly(&tmp)
        .args(["run", "--prompt", "x"])
        .assert()
        .success();

    assert_eq!(before, git(&tmp, &["rev-parse", "HEAD"]), "HEAD moved");
    // Full porcelain, untracked files included: nothing a job does may show
    // up here, `.assembly/` included.
    assert_eq!(git(&tmp, &["status", "--porcelain"]), "");

    discard_outside_state(&tmp);
}

#[test]
fn container_flags_are_refused_for_the_local_runner() {
    let root = tempfile::tempdir().unwrap();

    Command::cargo_bin("assembly")
        .unwrap()
        .env("ASSEMBLY_ROOT", root.path())
        .args(["daemon", "--pass-env", "ANTHROPIC_API_KEY"])
        .assert()
        .code(2)
        .stderr(contains("container runners"));
}

#[test]
fn the_k8s_runner_requires_a_namespace() {
    let root = tempfile::tempdir().unwrap();

    Command::cargo_bin("assembly")
        .unwrap()
        .env("ASSEMBLY_ROOT", root.path())
        .args(["daemon", "--runner", "k8s"])
        .assert()
        .code(2)
        .stderr(contains("--namespace"));
}

#[test]
fn a_namespace_is_refused_for_runners_that_have_none() {
    let root = tempfile::tempdir().unwrap();

    Command::cargo_bin("assembly")
        .unwrap()
        .env("ASSEMBLY_ROOT", root.path())
        .args(["daemon", "--runner", "docker", "--namespace", "factory"])
        .assert()
        .code(2)
        .stderr(contains("k8s runner"));
}

fn inapplicable_flags_of(argv: &[&str]) -> Option<InapplicableFlags> {
    match Cli::try_parse_from(argv).unwrap().command {
        Subcommand::Daemon { runner, .. } => runner.inapplicable_flags(),
        other => panic!("not a command that takes a runner: {other:?}"),
    }
}

#[test]
fn a_kubectl_context_is_inapplicable_to_docker() {
    assert_eq!(
        inapplicable_flags_of(&["assembly", "daemon", "--runner", "docker", "--context", "c"]),
        Some(InapplicableFlags::KubernetesOnly)
    );
}

#[test]
fn pass_env_is_inapplicable_to_the_local_runner() {
    assert_eq!(
        inapplicable_flags_of(&["assembly", "daemon", "--pass-env", "KEY"]),
        Some(InapplicableFlags::ContainerOnly)
    );
}

#[test]
fn every_flag_applies_to_the_k8s_runner() {
    assert_eq!(
        inapplicable_flags_of(&[
            "assembly",
            "daemon",
            "--runner",
            "k8s",
            "--namespace",
            "n",
            "--context",
            "c",
            "--image",
            "i",
            "--pass-env",
            "KEY",
        ]),
        None
    );
}
