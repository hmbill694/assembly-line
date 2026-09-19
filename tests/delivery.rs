use assembly_line::config::{Delivery, DeliveryMode, RepoConfig};
use assembly_line::delivery::{Delivered, deliver};
use assembly_line::git::{self, commit_all, head_sha};
use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use std::path::PathBuf;

mod support;

/// A repository with one commit, and optionally a bare sibling as `origin`.
/// No network, no credentials, no `gh`.
struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    origin: PathBuf,
}

impl Fixture {
    async fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        support::init_git_repo(&repo).await;

        Fixture {
            origin: tmp.path().join("origin.git"),
            _tmp: tmp,
            repo,
        }
    }

    async fn with_origin(&self) -> &PathBuf {
        support::add_origin(&self.repo, &self.origin).await;
        // The base has to exist on the remote before anything can land on it.
        git::push_branch(&self.repo, "origin", "main")
            .await
            .unwrap();
        &self.origin
    }

    /// A job branch one commit ahead of main.
    async fn job_branch(&self, name: &str) -> String {
        let base = head_sha(&self.repo).await.unwrap();
        let wt = self.repo.parent().unwrap().join(format!("wt-{name}"));
        git::add_worktree(&self.repo, &wt, name, &base)
            .await
            .unwrap();
        std::fs::write(wt.join("work.txt"), "the job's work\n").unwrap();
        let sha = commit_all(&wt, "job work").await.unwrap().unwrap();
        git::remove_worktree(&self.repo, &wt).await.unwrap();
        sha
    }
}

fn mode(mode: DeliveryMode) -> Delivery {
    Delivery { mode }
}

#[tokio::test]
async fn delivery_is_skipped_when_it_is_turned_off() {
    let fx = Fixture::new().await;
    fx.with_origin().await;
    fx.job_branch("al/job-1").await;

    let outcome = deliver(
        &fx.repo,
        &mode(DeliveryMode::None),
        "origin",
        "al/job-1",
        "main",
    )
    .await
    .unwrap();

    assert!(matches!(outcome, Delivered::Skipped(_)), "{outcome:?}");
}

/// A repository with no remote is an ordinary local job, not a failure. The
/// branch simply stays where it is.
#[tokio::test]
async fn delivery_without_a_remote_is_skipped_rather_than_failing() {
    let fx = Fixture::new().await;
    fx.job_branch("al/job-1").await;

    let outcome = deliver(
        &fx.repo,
        &mode(DeliveryMode::Pr),
        "origin",
        "al/job-1",
        "main",
    )
    .await
    .unwrap();

    match outcome {
        Delivered::Skipped(why) => assert!(why.contains("origin"), "{why}"),
        other => panic!("expected a skip, got {other:?}"),
    }
}

/// Turning delivery off must not push either: a skip is a skip.
#[tokio::test]
async fn delivery_turned_off_leaves_the_remote_alone() {
    let fx = Fixture::new().await;
    let origin = fx.with_origin().await.clone();
    fx.job_branch("al/job-1").await;

    deliver(
        &fx.repo,
        &mode(DeliveryMode::None),
        "origin",
        "al/job-1",
        "main",
    )
    .await
    .unwrap();

    let on_remote = git::run_allowing_failure(&origin, &["rev-parse", "al/job-1"])
        .await
        .unwrap();
    assert!(
        !on_remote.succeeded(),
        "the branch reached the remote despite delivery being off"
    );
}

/// `gh` may or may not be installed, and delivery must not depend on it: the
/// branch reaching the remote is the part that matters, and is what this
/// asserts. Which of the two `Delivered` variants comes back is not pinned,
/// because that depends on the machine.
#[tokio::test]
async fn pr_mode_pushes_the_branch_whether_or_not_gh_opens_a_pull_request() {
    let fx = Fixture::new().await;
    let origin = fx.with_origin().await.clone();
    let job_sha = fx.job_branch("al/job-1").await;

    let outcome = deliver(
        &fx.repo,
        &mode(DeliveryMode::Pr),
        "origin",
        "al/job-1",
        "main",
    )
    .await
    .unwrap();

    // Either `gh` opened one, or it could not — both leave the branch pushed.
    assert!(
        matches!(outcome, Delivered::Opened { .. } | Delivered::Pushed { .. }),
        "{outcome:?}"
    );

    let on_remote = git::run_allowing_failure(&origin, &["rev-parse", "al/job-1"])
        .await
        .unwrap();
    assert_eq!(on_remote.stdout.trim(), job_sha);
    // The base is untouched: a pull request is a request, not a merge.
    let base = git::run_allowing_failure(&origin, &["rev-parse", "main"])
        .await
        .unwrap();
    assert_ne!(base.stdout.trim(), job_sha);
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

/// Where this test's worktrees go, kept out of the shared `$HOME` default so
/// a leftover cannot confuse another test's `gc`.
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

/// A repository opted in with `verify` set to `verify`, running the given
/// fixture script.
async fn repo_running(script: &str, verify: &str) -> tempfile::TempDir {
    let tmp = support::repo_with_initial_commit().await;
    std::fs::create_dir_all(tmp.path().join(".assembly")).unwrap();
    std::fs::write(
        tmp.path().join(assembly_line::config::REPO_CONFIG_PATH),
        format!("verify = \"{verify}\"\n{}", support::config_running(script)),
    )
    .unwrap();
    commit_all(tmp.path(), "opt in").await.unwrap().unwrap();
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
    tmp
}

/// A bare sibling repository added as `origin`, so a real delivery would have
/// somewhere to push to.
async fn add_origin(tmp: &tempfile::TempDir) {
    support::add_origin(tmp.path(), &tmp.path().join("origin.git")).await;
    git::push_branch(tmp.path(), "origin", "main")
        .await
        .unwrap();
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
    add_origin(&tmp).await;

    assembly(&tmp)
        .args(["run", "--prompt", "write a file"])
        .assert()
        .code(1)
        .stdout(contains("not delivered"));

    discard_worktrees(&tmp);
}

/// The gate above is wired into `run`, but `revise` has its own call site
/// (`src/main.rs`'s `revise_existing_job`) which nothing else exercises — a
/// regression that dropped it would pass the whole suite. A passing revise
/// round must deliver just as a passing `run` does.
///
/// The assertion has to hold whether or not `gh` is installed on the test
/// machine, so it checks two things that are true either way: the printed
/// line is never "not delivered" (that only happens when the gate skips
/// delivery), and — decisively — the round's branch actually reached the
/// bare `origin`, which only happens once `deliver` runs.
#[tokio::test]
async fn a_passing_revise_round_is_delivered() {
    let tmp = repo_running("revising-agent.sh", "true").await;
    add_origin(&tmp).await;

    assembly(&tmp)
        .args(["run", "--prompt", "hi"])
        .assert()
        .success();

    assembly(&tmp)
        .args(["revise", "1", "add error handling"])
        .assert()
        .success()
        .stdout(contains("round 2"))
        .stdout(contains("pushed").or(contains("opened")));

    let local = git::run_allowing_failure(tmp.path(), &["rev-parse", "al/job-1"])
        .await
        .unwrap();
    let on_remote =
        git::run_allowing_failure(&tmp.path().join("origin.git"), &["rev-parse", "al/job-1"])
            .await
            .unwrap();
    assert_eq!(
        on_remote.stdout.trim(),
        local.stdout.trim(),
        "revise's branch never reached the remote — delivery is not wired into revise"
    );

    discard_worktrees(&tmp);
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
    add_origin(&tmp).await;
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

    discard_worktrees(&tmp);
}
