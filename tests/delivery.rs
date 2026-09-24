use assembly_line::config::{Delivery, DeliveryMode, RepoConfig};
use assembly_line::delivery::{Delivered, PullRequestText, deliver};
use assembly_line::git::{self, commit_all};
use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use std::path::PathBuf;

mod support;

/// A repository with one commit. No network, no credentials, no `gh`.
struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
}

impl Fixture {
    async fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        support::init_git_repo(&repo).await;

        Fixture { _tmp: tmp, repo }
    }
}

fn mode(mode: DeliveryMode) -> Delivery {
    Delivery { mode }
}

#[tokio::test]
async fn delivery_is_skipped_when_it_is_turned_off() {
    let fx = Fixture::new().await;
    let outcome = deliver(
        &fx.repo,
        &mode(DeliveryMode::None),
        "al/job-1",
        "main",
        PullRequestText {
            title: "job 1: x",
            body: "x",
        },
    )
    .await;
    assert!(matches!(outcome, Delivered::Skipped(_)), "{outcome:?}");
}

#[test]
fn delivery_defaults_to_a_pull_request_and_the_branch_you_started_from() {
    let config = RepoConfig::parse("provider = \"p\"\n").unwrap();

    assert_eq!(config.delivery.mode, DeliveryMode::Pr);
    assert_eq!(
        config.base, None,
        "an unset base means the branch the job started from, never an assumed main"
    );
}

#[test]
fn delivery_is_configurable_from_the_repositorys_own_config() {
    let config = RepoConfig::parse("base = \"develop\"\n[delivery]\nmode = \"none\"\n").unwrap();

    assert_eq!(config.delivery.mode, DeliveryMode::None);
}

// The line below prints only from `src/main.rs`, once a job's outcome is
// known — the library's `Harness` has no stdout to assert on — so this one
// test drives the real binary, the way `tests/cli.rs` does.

fn assembly(tmp: &tempfile::TempDir) -> Command {
    let mut cmd = Command::cargo_bin("assembly").unwrap();
    cmd.current_dir(tmp.path());
    cmd
}

/// Where this test's bare remote lives: outside the repository, so the
/// repository's own `git status` stays clean.
fn origin_for(tmp: &tempfile::TempDir) -> PathBuf {
    std::env::temp_dir().join(format!(
        "assembly-test-origin-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    ))
}

/// The bare remote lives outside the tempdir, so a test that makes one has to
/// take it with it.
fn discard_origin(tmp: &tempfile::TempDir) {
    let _ = std::fs::remove_dir_all(origin_for(tmp));
}

/// A repository opted in with `verify` set to `verify`, running the given
/// fixture script, with `main` published to its origin.
async fn repo_running(script: &str, verify: &str) -> tempfile::TempDir {
    let tmp = support::repo_with_initial_commit().await;
    std::fs::create_dir_all(tmp.path().join(".assembly")).unwrap();
    std::fs::write(
        tmp.path().join(assembly_line::config::REPO_CONFIG_PATH),
        format!("verify = \"{verify}\"\n{}", support::config_running(script)),
    )
    .unwrap();
    commit_all(tmp.path(), "opt in").await.unwrap().unwrap();
    support::add_origin(tmp.path(), &origin_for(&tmp)).await;
    support::publish_main(tmp.path()).await;
    tmp
}

/// Like [`repo_running`], but also declares `base`, so a test can assert on
/// where delivery targets its pull request.
async fn repo_running_with_base(script: &str, verify: &str, base: &str) -> tempfile::TempDir {
    let tmp = support::repo_with_initial_commit().await;
    std::fs::create_dir_all(tmp.path().join(".assembly")).unwrap();
    std::fs::write(
        tmp.path().join(assembly_line::config::REPO_CONFIG_PATH),
        format!(
            "verify = \"{verify}\"\nbase = \"{base}\"\n{}",
            support::config_running(script)
        ),
    )
    .unwrap();
    commit_all(tmp.path(), "opt in").await.unwrap().unwrap();
    support::add_origin(tmp.path(), &origin_for(&tmp)).await;
    support::publish_main(tmp.path()).await;
    tmp
}

/// A fake `gh` that records exactly what it was invoked with instead of
/// touching the network, so a test can assert on `--base` directly rather
/// than inferring it from which `Delivered` variant came back. Returns the
/// directory to prepend to `PATH` and the file its invocation is recorded
/// to.
fn fake_gh_capturing_args(tmp: &tempfile::TempDir) -> (PathBuf, PathBuf) {
    let bin_dir = tmp.path().join("fake-bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let capture = tmp.path().join("gh-invocation.txt");
    let script = bin_dir.join("gh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho \"$@\" >> {}\necho https://example.invalid/pr/1\n",
            capture.display()
        ),
    )
    .unwrap();
    let mut perms = std::fs::metadata(&script).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&script, perms).unwrap();
    (bin_dir, capture)
}

/// The gate this task implements: a job's branch is real work either way, but
/// a pull request for work that failed `verify` is noise. If delivery were
/// not gated on `failed`, this job's branch would reach `deliver` in `Pr`
/// mode; with no `gh` on the test machine, that prints "pushed ... no pull
/// request opened", never "not delivered" — so this assertion only passes
/// when the gate is in place.
#[tokio::test]
async fn a_failed_job_is_not_delivered() {
    let tmp = repo_running("fake-agent.sh", "exit 1").await;

    assembly(&tmp)
        .args(["run", "--prompt", "write a file"])
        .assert()
        .code(1)
        .stdout(contains("not delivered"));

    discard_origin(&tmp);
}

/// The gate above is wired into `run`, but `revise` has its own call site
/// (`src/main.rs`'s `revise_existing_job`) which nothing else exercises — a
/// regression that dropped it would pass the whole suite. A passing revise
/// round must deliver just as a passing `run` does.
///
/// The assertion has to hold whether or not `gh` is installed on the test
/// machine, so it checks what is true either way: the printed line says the
/// branch was pushed or a pull request opened — never "not delivered", which
/// only happens when the gate skips delivery — and the remote's branch
/// carries both rounds.
#[tokio::test]
async fn a_passing_revise_round_is_delivered() {
    let tmp = repo_running("revising-agent.sh", "true").await;

    assembly(&tmp)
        .args(["run", "--prompt", "hi"])
        .assert()
        .success();

    assembly(&tmp)
        .args(["revise", "1", "add error handling"])
        .assert()
        .success()
        .stdout(contains("round 2"))
        .stdout(contains("pushed").or(contains("opened")))
        .stdout(contains("not delivered").not());

    let rounds = git::file_at_ref(origin_for(&tmp), "al/job-1", "rounds.txt")
        .await
        .unwrap()
        .unwrap_or_default();
    // The revise prompt is several lines itself, so both rounds show as the
    // first round's line followed by the feedback, not as a line count.
    assert!(
        rounds.starts_with("hi\n") && rounds.contains("add error handling"),
        "the remote's branch does not carry both rounds: {rounds}"
    );

    discard_origin(&tmp);
}

/// `config.base` is consulted at exactly one place — `deliver_if_verified`
/// choosing the pull request's base — and until now nothing proved it
/// actually got there; every existing test on `base` only asserts that it
/// parses. This drives the real binary with a fake `gh` standing in for the
/// real one, so the `--base` a pull request would open with is captured
/// directly instead of inferred from which `Delivered` variant printed.
///
/// It also proves the divergence note fires: `base = "release"` here while
/// the job is cut from the default checked-out branch, `main` — exactly the
/// silent-scope-creep case the review found.
#[tokio::test]
async fn configured_base_reaches_the_pull_request_and_the_divergence_is_reported() {
    let tmp = repo_running_with_base("fake-agent.sh", "true", "release").await;
    let (fake_bin, gh_capture) = fake_gh_capturing_args(&tmp);
    let path_with_fake_gh = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    assembly(&tmp)
        .env("PATH", path_with_fake_gh)
        .args(["run", "--prompt", "write a file"])
        .assert()
        .success()
        .stdout(contains("will target 'release'"))
        .stdout(contains("cut from 'main'"));

    let invocation = std::fs::read_to_string(&gh_capture)
        .expect("gh was never invoked — the configured base never reached delivery");
    assert!(
        invocation.contains("--base release"),
        "gh was not asked to target the configured base: {invocation}"
    );
    assert!(
        invocation.contains("--head al/job-1"),
        "gh was not asked to deliver the job's own branch: {invocation}"
    );

    discard_origin(&tmp);
}

/// `gh pr create --fill` works out a title from the branch's commits in the
/// local repository, which never has a job's branch — so the pull request is
/// told what the job was asked to do instead.
#[tokio::test]
async fn a_pull_request_is_titled_and_described_from_the_job_not_from_local_commits() {
    let tmp = repo_running("fake-agent.sh", "true").await;
    let (fake_bin, gh_capture) = fake_gh_capturing_args(&tmp);
    let path_with_fake_gh = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    assembly(&tmp)
        .env("PATH", path_with_fake_gh)
        .args(["run", "--prompt", "write a file"])
        .assert()
        .success();

    let invocation = std::fs::read_to_string(&gh_capture).unwrap();
    assert!(
        invocation.contains("--title job 1: write a file --body write a file"),
        "{invocation}"
    );
    assert!(!invocation.contains("--fill"), "{invocation}");

    discard_origin(&tmp);
}
