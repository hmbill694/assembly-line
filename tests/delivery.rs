use assembly_line::config::{Delivery, DeliveryMode, RepoConfig};
use assembly_line::delivery::{Delivered, deliver};
use assembly_line::git::{self, commit_all, head_sha};
use std::path::PathBuf;

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

        Fixture {
            origin: tmp.path().join("origin.git"),
            _tmp: tmp,
            repo,
        }
    }

    async fn with_origin(&self) -> &PathBuf {
        let arg = self.origin.to_string_lossy().into_owned();
        for args in [
            vec!["init", "--bare", "--initial-branch=main", &arg],
            vec!["remote", "add", "origin", &arg],
        ] {
            let out = git::run_allowing_failure(&self.repo, &args).await.unwrap();
            assert!(out.succeeded(), "git {args:?}: {}", out.stderr);
        }
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
    Delivery { mode, base: None }
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

/// `gh` is not installed in CI, and delivery must not depend on it: the branch
/// reaching the remote is the part that matters.
#[tokio::test]
async fn pr_mode_still_pushes_when_no_pull_request_can_be_opened() {
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
        config.delivery.base, None,
        "an unset base means the branch the job started from, never an assumed main"
    );
}

#[test]
fn delivery_is_configurable_from_the_repositorys_own_config() {
    let config = RepoConfig::parse("[delivery]\nmode = \"none\"\nbase = \"develop\"\n").unwrap();

    assert_eq!(config.delivery.mode, DeliveryMode::None);
    assert_eq!(config.delivery.base.as_deref(), Some("develop"));
}
