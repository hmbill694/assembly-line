use assembly_line::git::{self, PinnedRef, head_sha};
use assembly_line::workspace;
use std::path::PathBuf;

mod support;

/// A repository published to a bare origin, and a scratch root the test owns.
struct Fixture {
    tmp: tempfile::TempDir,
    repo: PathBuf,
    origin: PathBuf,
}

impl Fixture {
    async fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        support::init_git_repo(&repo).await;
        let origin = tmp.path().join("origin.git");
        support::add_origin(&repo, &origin).await;
        support::publish_main(&repo).await;
        Fixture { tmp, repo, origin }
    }

    fn url(&self) -> &str {
        self.origin.to_str().unwrap()
    }

    fn scratch(&self) -> PathBuf {
        self.tmp.path().join("scratch")
    }

    async fn main(&self) -> PinnedRef {
        git::pinned(&self.repo, "origin", "main").await.unwrap()
    }

    async fn workspace(&self) -> anyhow::Result<workspace::RoundWorkspace> {
        workspace::create(
            self.url(),
            &self.main().await,
            "al/job-1",
            self.scratch(),
            None,
        )
        .await
    }
}

#[tokio::test]
async fn creating_a_workspace_checks_out_the_pinned_commit_on_the_jobs_branch() {
    let fx = Fixture::new().await;
    let ws = fx.workspace().await.unwrap();

    assert_eq!(head_sha(ws.path()).await.unwrap(), fx.main().await.sha);
    assert_eq!(
        git::current_branch(ws.path()).await.unwrap().as_deref(),
        Some("al/job-1")
    );
    assert!(ws.path().starts_with(fx.scratch()));
}

/// Agent work is never attributed to a person — not the repository's
/// configured author, nor whoever's global config the clone happens to see.
#[tokio::test]
async fn a_workspace_commits_as_assembly_line() {
    let fx = Fixture::new().await;
    let ws = fx.workspace().await.unwrap();

    std::fs::write(ws.path().join("work.txt"), "did the work\n").unwrap();
    workspace::commit(&ws, "work").await.unwrap().unwrap();

    let author = git::run_allowing_failure(ws.path(), &["log", "-1", "--format=%an <%ae>"])
        .await
        .unwrap()
        .stdout;
    assert_eq!(author.trim(), "assembly-line <assembly-line@localhost>");
}

#[tokio::test]
async fn committing_an_untouched_workspace_produces_nothing() {
    let fx = Fixture::new().await;
    let ws = fx.workspace().await.unwrap();

    assert!(
        workspace::commit(&ws, "nothing happened")
            .await
            .unwrap()
            .is_none(),
        "an agent may correctly decide no change is needed"
    );
}

#[tokio::test]
async fn discarding_a_workspace_removes_it_and_the_published_branch_survives() {
    let fx = Fixture::new().await;
    let ws = fx.workspace().await.unwrap();
    std::fs::write(ws.path().join("work.txt"), "done\n").unwrap();
    let sha = workspace::commit(&ws, "work").await.unwrap().unwrap();
    workspace::publish(&ws).await.unwrap();
    let path = ws.path().to_path_buf();

    workspace::discard(ws).unwrap();

    assert!(!path.exists());
    let on_remote = git::run_allowing_failure(&fx.origin, &["rev-parse", "al/job-1"])
        .await
        .unwrap();
    assert_eq!(on_remote.stdout.trim(), sha);
}

/// What a revise round does: the checkout from the last round is long gone,
/// but continuing the branch puts that work back on disk. This is the whole
/// mechanism by which an agent revises rather than restarts — no checkout had
/// to be kept alive to make it happen.
#[tokio::test]
async fn continuing_a_branch_restores_the_previous_rounds_work() {
    let fx = Fixture::new().await;
    let first = fx.workspace().await.unwrap();
    std::fs::write(first.path().join("rounds.txt"), "one\n").unwrap();
    workspace::commit(&first, "round 1").await.unwrap().unwrap();
    workspace::publish(&first).await.unwrap();
    workspace::discard(first).unwrap();

    let tip = git::pinned(&fx.repo, "origin", "al/job-1").await.unwrap();
    let second = workspace::create(fx.url(), &tip, "al/job-1", fx.scratch(), None)
        .await
        .unwrap();

    assert_eq!(
        std::fs::read_to_string(second.path().join("rounds.txt")).unwrap(),
        "one\n",
        "the agent cannot revise work it cannot see"
    );
}

/// The clone is the agent's to write, hooks included, and the push runs
/// with the git token in its environment — so the push runs no hook the
/// clone carries.
#[tokio::test]
async fn publishing_runs_no_hook_from_the_clone() {
    use std::os::unix::fs::PermissionsExt;

    let fx = Fixture::new().await;
    let ws = fx.workspace().await.unwrap();
    let marker = fx.tmp.path().join("pre-push-ran");
    let hook = ws.path().join(".git/hooks/pre-push");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(
        &hook,
        format!("#!/bin/sh\ntouch '{}'\nexit 0\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(ws.path().join("work.txt"), "done\n").unwrap();
    let sha = workspace::commit(&ws, "work").await.unwrap().unwrap();

    workspace::publish(&ws).await.unwrap();

    assert!(!marker.exists(), "the clone's pre-push hook ran");
    let on_remote = git::run_allowing_failure(&fx.origin, &["rev-parse", "al/job-1"])
        .await
        .unwrap();
    assert_eq!(on_remote.stdout.trim(), sha);
}

#[tokio::test]
async fn a_pinned_commit_is_used_even_after_its_ref_moves() {
    let fx = Fixture::new().await;
    let pinned = fx.main().await;
    std::fs::write(fx.repo.join("later.txt"), "later\n").unwrap();
    support::commit_all(&fx.repo, "later")
        .await
        .unwrap()
        .unwrap();
    support::publish_main(&fx.repo).await;

    let clone = workspace::clone_scratch(fx.url(), fx.scratch(), None)
        .await
        .unwrap();
    let base = workspace::pin_in_clone(&clone, "main", Some(&pinned.sha))
        .await
        .unwrap();

    assert_eq!(base, pinned);
}

#[tokio::test]
async fn a_pin_the_remote_cannot_supply_is_an_error() {
    let fx = Fixture::new().await;
    let clone = workspace::clone_scratch(fx.url(), fx.scratch(), None)
        .await
        .unwrap();

    let err = workspace::pin_in_clone(&clone, "main", Some(&"0".repeat(40)))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("not a commit"), "{err}");
}

#[tokio::test]
async fn a_job_branch_the_remote_lacks_is_none_not_an_error() {
    let fx = Fixture::new().await;
    let clone = workspace::clone_scratch(fx.url(), fx.scratch(), None)
        .await
        .unwrap();

    assert_eq!(
        workspace::job_branch_in_clone(&clone, "al/job-9")
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn a_refused_publish_names_the_branch_and_what_was_lost() {
    let fx = Fixture::new().await;
    let ws = fx.workspace().await.unwrap();
    std::fs::write(ws.path().join("work.txt"), "done\n").unwrap();
    workspace::commit(&ws, "work").await.unwrap().unwrap();
    // A remote that no longer exists refuses every attempt.
    std::fs::remove_dir_all(&fx.origin).unwrap();

    let err = workspace::publish(&ws).await.unwrap_err().to_string();
    assert!(err.contains("al/job-1"), "{err}");
    assert!(err.contains("work is lost"), "{err}");
}
