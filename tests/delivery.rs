use assembly_line::config::{Delivery, DeliveryMode, RepoConfig};
use assembly_line::delivery::{Delivered, PullRequestText, deliver};
use assembly_line::git;
use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use std::path::{Path, PathBuf};
use support::commit_all;

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

// The tests below assert on what a finished job prints, so they drive the
// real binary, the way `tests/cli.rs` does.

fn assembly(tmp: &tempfile::TempDir) -> Command {
    let mut cmd = Command::cargo_bin("assembly").unwrap();
    cmd.current_dir(tmp.path())
        .env("PATH", support::path_where_gh_refuses())
        .env("ASSEMBLY_ROOT", root_for(tmp));
    cmd
}

/// Where this test's job state lives: outside the repository, which a job
/// must leave untouched.
fn root_for(tmp: &tempfile::TempDir) -> PathBuf {
    std::env::temp_dir().join(format!(
        "assembly-test-root-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    ))
}

/// Where this test's bare remote lives: outside the repository, so the
/// repository's own `git status` stays clean.
fn origin_for(tmp: &tempfile::TempDir) -> PathBuf {
    std::env::temp_dir().join(format!(
        "assembly-test-origin-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    ))
}

/// The bare remote and the state root live outside the tempdir, so a test
/// that makes them has to take them with it.
fn discard_outside_state(tmp: &tempfile::TempDir) {
    let _ = std::fs::remove_dir_all(origin_for(tmp));
    let _ = std::fs::remove_dir_all(root_for(tmp));
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
///
/// `gh pr view` finds a pull request only when `$FAKE_GH_EXISTING` names
/// one. It is open unless `$FAKE_GH_EXISTING_STATE` says otherwise, and like
/// the real `gh`, a `--jq` that selects open pull requests prints nothing for
/// any other.
fn fake_gh_capturing_args(tmp: &tempfile::TempDir) -> (PathBuf, PathBuf) {
    let bin_dir = tmp.path().join("fake-bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let capture = tmp.path().join("gh-invocation.txt");
    let script = bin_dir.join("gh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho \"$@\" >> {}\n\
             if [ \"$1 $2\" = \"pr view\" ]; then\n\
               [ -n \"${{FAKE_GH_EXISTING:-}}\" ] || exit 1\n\
               case \"$*\" in\n\
                 *'.state == \"OPEN\"'*) [ \"${{FAKE_GH_EXISTING_STATE:-OPEN}}\" = OPEN ] || exit 0 ;;\n\
               esac\n\
               echo \"$FAKE_GH_EXISTING\"; exit 0\n\
             fi\n\
             echo https://example.invalid/pr/1\n",
            capture.display()
        ),
    )
    .unwrap();
    let mut perms = std::fs::metadata(&script).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&script, perms).unwrap();
    (bin_dir, capture)
}

fn path_with(dir: &Path) -> String {
    format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

/// Delivery is gated on `verify`: a job's branch is real work either way, but
/// a pull request for work that failed `verify` is noise. The `gh` here
/// records every call, so a skipped gate would leave a record behind.
#[tokio::test]
async fn a_failed_round_is_not_delivered() {
    let tmp = repo_running("fake-agent.sh", "exit 1").await;
    let (fake_bin, gh_capture) = fake_gh_capturing_args(&tmp);

    assembly(&tmp)
        .env("PATH", path_with(&fake_bin))
        .args(["submit", "--prompt", "write a file"])
        .assert()
        .code(1)
        .stdout(contains("branch: al/job-1"))
        .stdout(contains("pull request:").not());

    assert!(
        !gh_capture.exists(),
        "a failed round reached gh: {:?}",
        std::fs::read_to_string(&gh_capture)
    );
    discard_outside_state(&tmp);
}

/// A passing revise round must deliver just as a passing new job does, and
/// `submit` reports the pull request its `run` recorded.
#[tokio::test]
async fn a_passing_revise_round_is_delivered() {
    let tmp = repo_running("revising-agent.sh", "true").await;
    let (fake_bin, gh_capture) = fake_gh_capturing_args(&tmp);

    assembly(&tmp)
        .env("PATH", path_with(&fake_bin))
        .args(["submit", "--prompt", "hi"])
        .assert()
        .success();

    assembly(&tmp)
        .env("PATH", path_with(&fake_bin))
        .args(["submit", "--job", "1", "--prompt", "add error handling"])
        .assert()
        .success()
        .stdout(contains("round 2"))
        .stdout(contains("pull request: https://example.invalid/pr/1"));

    let creates = std::fs::read_to_string(&gh_capture)
        .unwrap()
        .lines()
        .filter(|call| call.starts_with("pr create"))
        .count();
    assert_eq!(creates, 2, "the revise round never reached delivery");

    let rounds = git::file_at_ref(origin_for(&tmp), "al/job-1", "rounds.txt")
        .await
        .unwrap()
        .unwrap_or_default();
    assert_eq!(
        rounds, "hi\nadd error handling\n",
        "the remote's branch does not carry both rounds"
    );

    discard_outside_state(&tmp);
}

/// `config.base` is consulted at exactly one place — `run::deliver`
/// choosing the pull request's base. This drives the real binary with a fake
/// `gh` standing in for the real one, so the `--base` a pull request would
/// open with is captured directly instead of inferred from which `Delivered`
/// variant printed.
///
/// It also proves the divergence note fires: `base = "release"` here while
/// the job is cut from the default checked-out branch, `main`. `run` prints
/// it with the rest of the round's output, which is the job's log.
#[tokio::test]
async fn configured_base_reaches_the_pull_request_and_the_divergence_is_reported() {
    let tmp = repo_running_with_base("fake-agent.sh", "true", "release").await;
    let (fake_bin, gh_capture) = fake_gh_capturing_args(&tmp);

    assembly(&tmp)
        .env("PATH", path_with(&fake_bin))
        .args(["submit", "--prompt", "write a file"])
        .assert()
        .success();
    assembly(&tmp)
        .args(["logs", "1"])
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

    discard_outside_state(&tmp);
}

/// `gh pr create --fill` works out a title from the branch's commits in the
/// local repository, which never has a job's branch — so the pull request is
/// told what the job was asked to do instead.
#[tokio::test]
async fn a_pull_request_is_titled_and_described_from_the_job_not_from_local_commits() {
    let tmp = repo_running("fake-agent.sh", "true").await;
    let (fake_bin, gh_capture) = fake_gh_capturing_args(&tmp);

    assembly(&tmp)
        .env("PATH", path_with(&fake_bin))
        .args(["submit", "--prompt", "write a file"])
        .assert()
        .success();

    let invocation = std::fs::read_to_string(&gh_capture).unwrap();
    assert!(
        invocation.contains("--title job 1: write a file --body write a file"),
        "{invocation}"
    );
    assert!(!invocation.contains("--fill"), "{invocation}");

    discard_outside_state(&tmp);
}

#[tokio::test]
async fn run_opens_the_pull_request_from_inside_its_clone() {
    let tmp = repo_running("fake-agent.sh", "true").await;
    let (gh_dir, args_log) = fake_gh_capturing_args(&tmp);

    assembly(&tmp)
        .env("PATH", path_with(&gh_dir))
        .args(["run", "--prompt", "add auth"])
        .assert()
        .success()
        .stdout(contains("pull request: https://example.invalid/pr/1"));

    let args = std::fs::read_to_string(&args_log).unwrap();
    assert!(
        args.contains("pr create --base main --head al/job-1"),
        "{args}"
    );

    discard_outside_state(&tmp);
}

/// A revise's commits land on the pull request its job already has, so
/// asking for another would only fail.
#[tokio::test]
async fn a_revise_reports_the_pull_request_it_already_has() {
    let tmp = repo_running("fake-agent.sh", "true").await;
    let (gh_dir, args_log) = fake_gh_capturing_args(&tmp);
    assembly(&tmp)
        .env("PATH", path_with(&gh_dir))
        .args(["run", "--prompt", "a"])
        .assert()
        .success();
    std::fs::write(&args_log, "").unwrap();

    assembly(&tmp)
        .env("PATH", path_with(&gh_dir))
        .env("FAKE_GH_EXISTING", "https://example.invalid/pr/1")
        .args(["run", "--job", "1", "--prompt", "b"])
        .assert()
        .success()
        .stdout(contains("pull request: https://example.invalid/pr/1"));

    let args = std::fs::read_to_string(&args_log).unwrap();
    assert!(args.contains("pr view al/job-1"), "{args}");
    assert!(!args.contains("pr create"), "{args}");

    discard_outside_state(&tmp);
}

/// A merged or closed pull request is not one the revise's commits land on,
/// so the revise asks for a new one.
#[tokio::test]
async fn a_revise_whose_pull_request_was_merged_opens_another() {
    let tmp = repo_running("fake-agent.sh", "true").await;
    let (gh_dir, args_log) = fake_gh_capturing_args(&tmp);
    assembly(&tmp)
        .env("PATH", path_with(&gh_dir))
        .args(["run", "--prompt", "a"])
        .assert()
        .success();

    assembly(&tmp)
        .env("PATH", path_with(&gh_dir))
        .env("FAKE_GH_EXISTING", "https://example.invalid/pr/1")
        .env("FAKE_GH_EXISTING_STATE", "MERGED")
        .args(["run", "--job", "1", "--prompt", "b"])
        .assert()
        .success();

    let args = std::fs::read_to_string(&args_log).unwrap();
    assert_eq!(args.matches("pr create").count(), 2, "{args}");

    discard_outside_state(&tmp);
}

/// An earlier round pushed work that `verify` rejected; this round changes
/// nothing and passes. The branch still holds work its base does not, so it
/// is delivered.
#[tokio::test]
async fn a_revise_that_passes_without_new_commits_delivers_the_earlier_work() {
    let tmp = repo_running("fake-agent.sh", "true").await;
    let accepted = tmp.path().join("accepted");
    std::fs::write(
        tmp.path().join(assembly_line::config::REPO_CONFIG_PATH),
        format!(
            "verify = \"test -f '{}'\"\n{}",
            accepted.display(),
            support::config_running("fake-agent.sh")
        ),
    )
    .unwrap();
    commit_all(tmp.path(), "verify reads a flag")
        .await
        .unwrap()
        .unwrap();
    support::publish_main(tmp.path()).await;
    let (gh_dir, args_log) = fake_gh_capturing_args(&tmp);
    assembly(&tmp)
        .env("PATH", path_with(&gh_dir))
        .args(["run", "--prompt", "same"])
        .assert()
        .code(1);
    std::fs::write(&accepted, "").unwrap();

    assembly(&tmp)
        .env("PATH", path_with(&gh_dir))
        .args(["run", "--job", "1", "--prompt", "same"])
        .assert()
        .success()
        .stdout(contains("pull request: https://example.invalid/pr/1"));

    let args = std::fs::read_to_string(&args_log).unwrap();
    assert!(
        args.contains("pr create --base main --head al/job-1"),
        "{args}"
    );

    discard_outside_state(&tmp);
}
