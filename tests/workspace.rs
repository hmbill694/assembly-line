use assembly_line::git::{self, commit_all, head_sha};
use assembly_line::workspace::{self, StartPoint, node_branch_name, run_branch_name};
use std::path::PathBuf;

struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    seed: PathBuf,
    wt_root: PathBuf,
}

impl Fixture {
    async fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let seed = tmp.path().join("seed");
        let wt_root = tmp.path().join("wt");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&seed).unwrap();
        std::fs::create_dir_all(&wt_root).unwrap();

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
            _tmp: tmp,
            repo,
            seed,
            wt_root,
        }
    }
}

#[test]
fn branch_names_are_flat_siblings() {
    assert_eq!(run_branch_name(42), "al/run-42");
    assert_eq!(node_branch_name(42, "impl-auth"), "al/run-42-impl-auth");
    assert!(
        !node_branch_name(42, "impl-auth").starts_with(&format!("{}/", run_branch_name(42))),
        "a node branch must not nest under the run branch; git forbids it"
    );
}

#[tokio::test]
async fn creating_a_workspace_checks_out_the_base_commit() {
    let fx = Fixture::new().await;
    let base = head_sha(&fx.repo).await.unwrap();

    let ws = workspace::create(
        &fx.repo,
        fx.wt_root.join("impl-auth"),
        "al/run-1-impl-auth",
        StartPoint::FreshBranch(&base),
        &fx.seed,
        &[],
    )
    .await
    .unwrap();

    assert!(ws.path.join("README.md").is_file());
    assert_eq!(ws.branch, "al/run-1-impl-auth");
    assert!(ws.seeded.is_empty());
    assert!(!fx.repo.join("should-not-exist").exists());
}

#[tokio::test]
async fn seeded_files_are_copied_in_and_kept_out_of_the_commit() {
    let fx = Fixture::new().await;
    std::fs::write(fx.seed.join(".env"), "API_KEY=hunter2\n").unwrap();
    let base = head_sha(&fx.repo).await.unwrap();

    let ws = workspace::create(
        &fx.repo,
        fx.wt_root.join("n"),
        "al/run-1-n",
        StartPoint::FreshBranch(&base),
        &fx.seed,
        &[".env".to_string()],
    )
    .await
    .unwrap();

    assert_eq!(
        std::fs::read_to_string(ws.path.join(".env")).unwrap(),
        "API_KEY=hunter2\n",
        "the agent must be able to read it"
    );

    std::fs::write(ws.path.join("work.txt"), "did the work\n").unwrap();
    workspace::commit(&ws, "node work").await.unwrap().unwrap();

    let tracked = git::run_allowing_failure(&ws.path, &["ls-files"])
        .await
        .unwrap()
        .stdout;
    assert!(tracked.contains("work.txt"), "{tracked}");
    assert!(
        !tracked.contains(".env"),
        "seeded secret was committed: {tracked}"
    );
}

#[tokio::test]
async fn seeding_preserves_nested_paths() {
    let fx = Fixture::new().await;
    std::fs::create_dir_all(fx.seed.join(".claude")).unwrap();
    std::fs::write(fx.seed.join(".claude/settings.local.json"), "{}\n").unwrap();
    let base = head_sha(&fx.repo).await.unwrap();

    let ws = workspace::create(
        &fx.repo,
        fx.wt_root.join("n"),
        "al/run-1-n",
        StartPoint::FreshBranch(&base),
        &fx.seed,
        &[".claude/settings.local.json".to_string()],
    )
    .await
    .unwrap();

    assert!(ws.path.join(".claude/settings.local.json").is_file());
}

#[tokio::test]
async fn a_missing_seed_path_names_the_file_and_leaves_no_worktree() {
    let fx = Fixture::new().await;
    let base = head_sha(&fx.repo).await.unwrap();
    let path = fx.wt_root.join("n");

    let err = workspace::create(
        &fx.repo,
        &path,
        "al/run-1-n",
        StartPoint::FreshBranch(&base),
        &fx.seed,
        &["nope.env".to_string()],
    )
    .await
    .unwrap_err()
    .to_string();

    assert!(err.contains("nope.env"), "{err}");
    assert!(!path.exists(), "a typo should cost nothing");
    assert!(!git::branch_exists(&fx.repo, "al/run-1-n").await.unwrap());
}

#[tokio::test]
async fn committing_an_untouched_workspace_produces_nothing() {
    let fx = Fixture::new().await;
    let base = head_sha(&fx.repo).await.unwrap();
    let ws = workspace::create(
        &fx.repo,
        fx.wt_root.join("n"),
        "al/run-1-n",
        StartPoint::FreshBranch(&base),
        &fx.seed,
        &[],
    )
    .await
    .unwrap();

    assert!(
        workspace::commit(&ws, "nothing happened")
            .await
            .unwrap()
            .is_none(),
        "an agent may correctly decide no change is needed"
    );
}

#[tokio::test]
async fn discarding_a_workspace_removes_it_but_keeps_the_branch() {
    let fx = Fixture::new().await;
    let base = head_sha(&fx.repo).await.unwrap();
    let ws = workspace::create(
        &fx.repo,
        fx.wt_root.join("n"),
        "al/run-1-n",
        StartPoint::FreshBranch(&base),
        &fx.seed,
        &[],
    )
    .await
    .unwrap();

    workspace::discard(&fx.repo, &ws).await.unwrap();

    assert!(!ws.path.exists());
    assert!(
        git::branch_exists(&fx.repo, "al/run-1-n").await.unwrap(),
        "the branch is the record of the work; only the checkout is disposable"
    );
}

/// What a revise round does: the checkout from the last round is long gone,
/// but continuing the branch puts that work back on disk. This is the whole
/// mechanism by which an agent revises rather than restarts — no worktree had
/// to be kept alive to make it happen.
#[tokio::test]
async fn continuing_a_branch_restores_the_previous_rounds_work() {
    let fx = Fixture::new().await;
    let base = head_sha(&fx.repo).await.unwrap();
    let path = fx.wt_root.join("n");

    let round_one = workspace::create(
        &fx.repo,
        &path,
        "al/run-1-n",
        StartPoint::FreshBranch(&base),
        &fx.seed,
        &[],
    )
    .await
    .unwrap();
    std::fs::write(round_one.path.join("work.txt"), "round one\n").unwrap();
    let first_sha = workspace::commit(&round_one, "round 1")
        .await
        .unwrap()
        .unwrap();
    workspace::discard(&fx.repo, &round_one).await.unwrap();
    assert!(!path.exists(), "the round's scratch is gone");

    let round_two = workspace::create(
        &fx.repo,
        &path,
        "al/run-1-n",
        StartPoint::ContinueBranch,
        &fx.seed,
        &[],
    )
    .await
    .unwrap();

    assert_eq!(
        std::fs::read_to_string(round_two.path.join("work.txt")).unwrap(),
        "round one\n",
        "the agent cannot revise work it cannot see"
    );
    assert_eq!(head_sha(&round_two.path).await.unwrap(), first_sha);
}

#[tokio::test]
async fn a_second_attempt_supersedes_the_worktree_and_branch_of_the_first() {
    // A run that died mid-node can orphan a checkout, and its branch outlives
    // that. Git will reuse neither name, so a fresh attempt has to clear both —
    // otherwise it fails on the sandbox instead of on the work.
    let fx = Fixture::new().await;
    let base = head_sha(&fx.repo).await.unwrap();
    let path = fx.wt_root.join("n");

    let first = workspace::create(
        &fx.repo,
        &path,
        "al/run-1-n",
        StartPoint::FreshBranch(&base),
        &fx.seed,
        &[],
    )
    .await
    .unwrap();
    std::fs::write(first.path.join("attempt.txt"), "first try\n").unwrap();
    workspace::commit(&first, "first attempt").await.unwrap();

    let second = workspace::create(
        &fx.repo,
        &path,
        "al/run-1-n",
        StartPoint::FreshBranch(&base),
        &fx.seed,
        &[],
    )
    .await
    .unwrap();

    assert!(
        !second.path.join("attempt.txt").exists(),
        "the second attempt inherited the first attempt's work"
    );
    assert_eq!(head_sha(&second.path).await.unwrap(), base);
}

#[tokio::test]
async fn a_branch_left_without_its_worktree_does_not_block_the_next_attempt() {
    // `gc` removes checkouts but never branches, so this is the state a
    // collected run leaves behind.
    let fx = Fixture::new().await;
    let base = head_sha(&fx.repo).await.unwrap();
    let path = fx.wt_root.join("n");

    let first = workspace::create(
        &fx.repo,
        &path,
        "al/run-1-n",
        StartPoint::FreshBranch(&base),
        &fx.seed,
        &[],
    )
    .await
    .unwrap();
    workspace::discard(&fx.repo, &first).await.unwrap();
    assert!(git::branch_exists(&fx.repo, "al/run-1-n").await.unwrap());

    let second = workspace::create(
        &fx.repo,
        &path,
        "al/run-1-n",
        StartPoint::FreshBranch(&base),
        &fx.seed,
        &[],
    )
    .await
    .unwrap();
    assert!(second.path.join("README.md").is_file());
}
