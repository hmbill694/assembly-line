use assembly_line::config::{Delivery, DeliveryMode, parse_graph};
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

    /// A run branch one commit ahead of main.
    async fn run_branch(&self, name: &str) -> String {
        let base = head_sha(&self.repo).await.unwrap();
        let wt = self.repo.parent().unwrap().join(format!("wt-{name}"));
        git::add_worktree(&self.repo, &wt, name, &base)
            .await
            .unwrap();
        std::fs::write(wt.join("work.txt"), "the run's work\n").unwrap();
        let sha = commit_all(&wt, "run work").await.unwrap().unwrap();
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
    fx.run_branch("al/run-1").await;

    let outcome = deliver(
        &fx.repo,
        &mode(DeliveryMode::None),
        "origin",
        "al/run-1",
        "main",
    )
    .await
    .unwrap();

    assert!(matches!(outcome, Delivered::Skipped(_)), "{outcome:?}");
}

/// A repository with no remote is an ordinary local run, not a failure. The
/// branch simply stays where it is.
#[tokio::test]
async fn delivery_without_a_remote_is_skipped_rather_than_failing() {
    let fx = Fixture::new().await;
    fx.run_branch("al/run-1").await;

    let outcome = deliver(
        &fx.repo,
        &mode(DeliveryMode::Pr),
        "origin",
        "al/run-1",
        "main",
    )
    .await
    .unwrap();

    match outcome {
        Delivered::Skipped(why) => assert!(why.contains("origin"), "{why}"),
        other => panic!("expected a skip, got {other:?}"),
    }
}

#[tokio::test]
async fn push_mode_advances_the_base_on_the_remote() {
    let fx = Fixture::new().await;
    let origin = fx.with_origin().await.clone();
    let run_sha = fx.run_branch("al/run-1").await;

    let outcome = deliver(
        &fx.repo,
        &mode(DeliveryMode::Push),
        "origin",
        "al/run-1",
        "main",
    )
    .await
    .unwrap();

    assert_eq!(
        outcome,
        Delivered::LandedOn {
            base: "main".into()
        }
    );

    let on_remote = git::run_allowing_failure(&origin, &["rev-parse", "main"])
        .await
        .unwrap();
    assert_eq!(
        on_remote.stdout.trim(),
        run_sha,
        "the base on the remote did not move to the run's work"
    );
}

/// A base that has moved on is a real conflict, and pushing is not forced —
/// so it is reported rather than silently overwriting someone else's commit.
#[tokio::test]
async fn push_mode_refuses_a_base_that_moved_underneath_it() {
    let fx = Fixture::new().await;
    let origin = fx.with_origin().await.clone();
    fx.run_branch("al/run-1").await;

    // Someone else lands on main first, from a separate clone.
    let other = fx.repo.parent().unwrap().join("other");
    let arg = origin.to_string_lossy().into_owned();
    git::run_allowing_failure(
        fx.repo.parent().unwrap(),
        &["clone", "-q", &arg, other.to_str().unwrap()],
    )
    .await
    .unwrap();
    for args in [
        vec!["config", "user.email", "o@e.com"],
        vec!["config", "user.name", "O"],
        vec!["config", "commit.gpgsign", "false"],
    ] {
        git::run_allowing_failure(&other, &args).await.unwrap();
    }
    std::fs::write(other.join("theirs.txt"), "not yours\n").unwrap();
    commit_all(&other, "someone else").await.unwrap().unwrap();
    git::push_branch(&other, "origin", "main").await.unwrap();

    let refused = deliver(
        &fx.repo,
        &mode(DeliveryMode::Push),
        "origin",
        "al/run-1",
        "main",
    )
    .await;

    assert!(
        refused.is_err(),
        "a non-fast-forward must not silently overwrite: {refused:?}"
    );
}

/// `gh` is not installed in CI, and delivery must not depend on it: the branch
/// reaching the remote is the part that matters.
#[tokio::test]
async fn pr_mode_still_pushes_when_no_pull_request_can_be_opened() {
    let fx = Fixture::new().await;
    let origin = fx.with_origin().await.clone();
    let run_sha = fx.run_branch("al/run-1").await;

    let outcome = deliver(
        &fx.repo,
        &mode(DeliveryMode::Pr),
        "origin",
        "al/run-1",
        "main",
    )
    .await
    .unwrap();

    // Either `gh` opened one, or it could not — both leave the branch pushed.
    assert!(
        matches!(outcome, Delivered::Opened { .. } | Delivered::Pushed { .. }),
        "{outcome:?}"
    );

    let on_remote = git::run_allowing_failure(&origin, &["rev-parse", "al/run-1"])
        .await
        .unwrap();
    assert_eq!(on_remote.stdout.trim(), run_sha);
    // The base is untouched: a pull request is a request, not a merge.
    let base = git::run_allowing_failure(&origin, &["rev-parse", "main"])
        .await
        .unwrap();
    assert_ne!(base.stdout.trim(), run_sha);
}

#[test]
fn delivery_defaults_to_a_pull_request_and_the_branch_you_started_from() {
    let graph = parse_graph("[[task]]\nid=\"a\"\n").unwrap();

    assert_eq!(graph.delivery.mode, DeliveryMode::Pr);
    assert_eq!(
        graph.delivery.base, None,
        "an unset base means the branch the run started from, never an assumed main"
    );
}

#[test]
fn delivery_is_configurable_from_the_graph() {
    let graph = parse_graph(
        "[delivery]\nmode = \"push\"\nbase = \"develop\"\n\
         [[task]]\nid=\"a\"\n",
    )
    .unwrap();

    assert_eq!(graph.delivery.mode, DeliveryMode::Push);
    assert_eq!(graph.delivery.base.as_deref(), Some("develop"));
}
