use assembly_line::git::{
    self, DiffStat, MergeOutcome, add_worktree, commit_all, commit_all_except, conflicted_paths,
    diff_stat_against, head_sha, is_dirty, merge_branch, remove_worktree,
};
use std::path::{Path, PathBuf};

/// A repository with one commit, plus a sibling directory for worktrees.
///
/// Both live under one tempdir so parallel tests never collide — worktrees
/// must sit outside the repo, but not in a shared location.
struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    worktrees: PathBuf,
}

impl Fixture {
    async fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let worktrees = tmp.path().join("wt");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&worktrees).unwrap();

        for args in [
            vec!["init", "--initial-branch=main"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Test"],
            vec!["config", "commit.gpgsign", "false"],
        ] {
            let out = git::run_allowing_failure(&repo, &args).await.unwrap();
            assert!(out.succeeded(), "git {args:?} failed: {}", out.stderr);
        }

        std::fs::write(repo.join("README.md"), "base\n").unwrap();
        commit_all(&repo, "initial")
            .await
            .unwrap()
            .expect("a commit");

        Fixture {
            _tmp: tmp,
            repo,
            worktrees,
        }
    }

    async fn base(&self) -> String {
        head_sha(&self.repo).await.unwrap()
    }

    /// Add a bare repository as `origin`. A push then exercises the real git
    /// path with no network and no credentials.
    async fn with_origin(&self) -> PathBuf {
        let origin = self.repo.parent().expect("a parent").join("origin.git");
        let origin_arg = origin.to_string_lossy().into_owned();

        for args in [
            vec!["init", "--bare", "--initial-branch=main", &origin_arg],
            vec!["remote", "add", "origin", &origin_arg],
        ] {
            let out = git::run_allowing_failure(&self.repo, &args).await.unwrap();
            assert!(out.succeeded(), "git {args:?} failed: {}", out.stderr);
        }
        origin
    }

    /// Check `branch` out into a fresh worktree started at the repo's HEAD.
    async fn worktree(&self, name: &str, branch: &str) -> PathBuf {
        let path = self.worktrees.join(name);
        let base = self.base().await;
        add_worktree(&self.repo, &path, branch, &base)
            .await
            .unwrap();
        path
    }
}

async fn write_and_commit(worktree: &Path, name: &str, body: &str, message: &str) -> String {
    std::fs::write(worktree.join(name), body).unwrap();
    commit_all(worktree, message)
        .await
        .unwrap()
        .expect("something to commit")
}

#[tokio::test]
async fn reports_head_and_current_branch() {
    let fx = Fixture::new().await;

    assert_eq!(head_sha(&fx.repo).await.unwrap().len(), 40);
    assert_eq!(
        git::current_branch(&fx.repo).await.unwrap().as_deref(),
        Some("main")
    );
    assert!(git::has_commits(&fx.repo).await.unwrap());
}

#[tokio::test]
async fn a_fresh_repo_has_no_commits() {
    let tmp = tempfile::tempdir().unwrap();
    git::run_allowing_failure(tmp.path(), &["init"])
        .await
        .unwrap();
    assert!(!git::has_commits(tmp.path()).await.unwrap());
}

#[tokio::test]
async fn commit_all_returns_none_when_the_tree_is_clean() {
    let fx = Fixture::new().await;
    assert!(
        commit_all(&fx.repo, "nothing to do")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_worktree_leaves_the_original_tree_untouched() {
    let fx = Fixture::new().await;
    let base = fx.base().await;
    let node = fx.worktree("node", "al/run-1-node").await;

    assert!(node.join("README.md").is_file());
    write_and_commit(&node, "new.txt", "from the node\n", "node work").await;

    assert!(!fx.repo.join("new.txt").exists());
    assert_eq!(head_sha(&fx.repo).await.unwrap(), base);
    assert_eq!(
        git::current_branch(&fx.repo).await.unwrap().as_deref(),
        Some("main")
    );
    assert!(git::branch_exists(&fx.repo, "al/run-1-node").await.unwrap());

    remove_worktree(&fx.repo, &node).await.unwrap();
    assert!(!node.exists());
}

/// Git refs are paths, so a run branch cannot be a directory prefix of its
/// node branches. This is why node branches are flat siblings.
#[tokio::test]
async fn a_node_branch_nested_under_the_run_branch_is_rejected_by_git() {
    let fx = Fixture::new().await;
    fx.worktree("run", "al/run-1").await;

    let base = fx.base().await;
    let nested = add_worktree(
        &fx.repo,
        fx.worktrees.join("nested"),
        "al/run-1/node",
        &base,
    )
    .await;

    assert!(
        nested.is_err(),
        "git accepted a ref nested under an existing ref"
    );
    // The flat sibling form is fine.
    assert!(fx.worktree("flat", "al/run-1-node").await.is_dir());
}

#[tokio::test]
async fn is_dirty_tracks_uncommitted_work() {
    let fx = Fixture::new().await;
    assert!(!is_dirty(&fx.repo).await.unwrap());

    std::fs::write(fx.repo.join("scratch.txt"), "wip").unwrap();
    assert!(is_dirty(&fx.repo).await.unwrap());
}

#[tokio::test]
async fn merging_a_node_branch_advances_the_integration_worktree_only() {
    let fx = Fixture::new().await;
    let integration = fx.worktree("run", "al/run-1").await;
    let node = fx.worktree("a", "al/run-1-a").await;

    write_and_commit(&node, "a.txt", "from a\n", "add a").await;
    let outcome = merge_branch(&integration, "al/run-1-a", "merge a")
        .await
        .unwrap();

    assert!(matches!(outcome, MergeOutcome::Merged(_)));
    assert!(integration.join("a.txt").is_file());
    assert!(!fx.repo.join("a.txt").exists(), "user tree was modified");
}

#[tokio::test]
async fn merging_twice_reports_already_up_to_date() {
    let fx = Fixture::new().await;
    let integration = fx.worktree("run", "al/run-1").await;
    let node = fx.worktree("a", "al/run-1-a").await;
    write_and_commit(&node, "a.txt", "from a\n", "add a").await;

    merge_branch(&integration, "al/run-1-a", "merge a")
        .await
        .unwrap();
    let second = merge_branch(&integration, "al/run-1-a", "merge a again")
        .await
        .unwrap();

    assert_eq!(second, MergeOutcome::AlreadyUpToDate);
}

#[tokio::test]
async fn two_nodes_touching_the_same_file_conflict_on_the_second_merge() {
    let fx = Fixture::new().await;
    let integration = fx.worktree("run", "al/run-1").await;

    for (dir, branch, body) in [
        ("a", "al/run-1-a", "written by a\n"),
        ("b", "al/run-1-b", "written by b\n"),
    ] {
        let node = fx.worktree(dir, branch).await;
        write_and_commit(&node, "shared.txt", body, "edit shared").await;
    }

    assert!(matches!(
        merge_branch(&integration, "al/run-1-a", "merge a")
            .await
            .unwrap(),
        MergeOutcome::Merged(_)
    ));

    let second = merge_branch(&integration, "al/run-1-b", "merge b")
        .await
        .unwrap();
    match second {
        MergeOutcome::Conflicted(paths) => assert_eq!(paths, vec!["shared.txt".to_string()]),
        other => panic!("expected a conflict, got {other:?}"),
    }

    // Left mid-merge on purpose: the markers are what a resolver acts on.
    let body = std::fs::read_to_string(integration.join("shared.txt")).unwrap();
    assert!(body.contains("<<<<<<<"), "{body}");
    assert_eq!(
        conflicted_paths(&integration).await.unwrap(),
        vec!["shared.txt".to_string()]
    );

    git::abort_merge(&integration).await.unwrap();
    assert!(conflicted_paths(&integration).await.unwrap().is_empty());
}

#[tokio::test]
async fn diff_stat_counts_files_and_lines_against_a_base() {
    let fx = Fixture::new().await;
    let base = fx.base().await;
    let node = fx.worktree("stat", "al/run-1-stat").await;

    std::fs::write(node.join("one.txt"), "a\nb\nc\n").unwrap();
    std::fs::write(node.join("README.md"), "base\nextra\n").unwrap();
    commit_all(&node, "changes").await.unwrap().unwrap();

    assert_eq!(
        diff_stat_against(&node, &base).await.unwrap(),
        DiffStat {
            files: 2,
            insertions: 4,
            deletions: 0
        }
    );
}

#[tokio::test]
async fn diff_stat_of_an_unchanged_worktree_is_empty() {
    let fx = Fixture::new().await;
    let base = fx.base().await;
    assert_eq!(
        diff_stat_against(&fx.repo, &base).await.unwrap(),
        DiffStat::default()
    );
}

#[tokio::test]
async fn seeded_files_stay_on_disk_but_out_of_the_commit() {
    let fx = Fixture::new().await;
    let node = fx.worktree("secret", "al/run-1-secret").await;

    std::fs::write(node.join(".env"), "API_KEY=hunter2\n").unwrap();
    std::fs::write(node.join("code.txt"), "real work\n").unwrap();

    commit_all_except(&node, "work", &[".env".to_string()])
        .await
        .unwrap()
        .expect("a commit");

    let tracked = git::run_allowing_failure(&node, &["ls-files"])
        .await
        .unwrap()
        .stdout;
    assert!(tracked.contains("code.txt"), "{tracked}");
    assert!(
        !tracked.contains(".env"),
        "the seeded secret was committed: {tracked}"
    );
    assert!(
        node.join(".env").is_file(),
        "the agent still needs to be able to read it"
    );
}

#[tokio::test]
async fn a_commit_containing_only_seeded_files_is_no_commit_at_all() {
    let fx = Fixture::new().await;
    let node = fx.worktree("only-secret", "al/run-1-only-secret").await;

    std::fs::write(node.join(".env"), "API_KEY=hunter2\n").unwrap();

    assert!(
        commit_all_except(&node, "work", &[".env".to_string()])
            .await
            .unwrap()
            .is_none(),
        "excluding the only change should leave nothing to commit"
    );
}

/// Unstaging protects the commits assembly-line makes. If the agent committed
/// the secret itself, that has to fail loudly rather than reach a remote.
#[tokio::test]
async fn a_secret_committed_by_the_agent_fails_the_node() {
    let fx = Fixture::new().await;
    let node = fx.worktree("agent-commit", "al/run-1-agent-commit").await;

    std::fs::write(node.join(".env"), "API_KEY=hunter2\n").unwrap();
    git::run_allowing_failure(&node, &["add", "-A"])
        .await
        .unwrap();
    git::run_allowing_failure(&node, &["commit", "--no-verify", "-m", "agent commit"])
        .await
        .unwrap();

    std::fs::write(node.join("more.txt"), "later work\n").unwrap();
    let err = commit_all_except(&node, "work", &[".env".to_string()])
        .await
        .unwrap_err()
        .to_string();

    assert!(err.contains(".env"), "{err}");
    assert_eq!(
        git::tracked_among(&node, &[".env".to_string()])
            .await
            .unwrap(),
        vec![".env".to_string()]
    );
}

#[tokio::test]
async fn a_failed_git_command_carries_gits_own_message() {
    let fx = Fixture::new().await;
    let err = head_sha(fx.repo.join("does-not-exist"))
        .await
        .unwrap_err()
        .to_string();
    assert!(!err.is_empty());
}

#[tokio::test]
async fn an_existing_branch_can_be_checked_out_into_a_fresh_worktree() {
    // A run's branch outlives its checkout whenever `gc` removes the directory
    // or a crash orphans it; resuming has to be able to pick the branch back up.
    let fx = Fixture::new().await;
    let node = fx.worktree("first", "al/run-1-node").await;
    let sha = write_and_commit(&node, "work.txt", "done\n", "node work").await;
    remove_worktree(&fx.repo, &node).await.unwrap();

    let again = fx.worktrees.join("again");
    git::add_worktree_for_existing_branch(&fx.repo, &again, "al/run-1-node")
        .await
        .unwrap();

    assert_eq!(
        head_sha(&again).await.unwrap(),
        sha,
        "lost the branch's work"
    );
    assert_eq!(
        std::fs::read_to_string(again.join("work.txt")).unwrap(),
        "done\n"
    );
}

#[tokio::test]
async fn checking_out_a_branch_that_is_already_in_a_worktree_is_refused() {
    // Two checkouts of one branch would let two nodes commit over each other.
    let fx = Fixture::new().await;
    fx.worktree("held", "al/run-1-node").await;

    assert!(
        git::add_worktree_for_existing_branch(
            &fx.repo,
            fx.worktrees.join("second"),
            "al/run-1-node"
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn deleting_a_branch_removes_it_even_though_it_was_never_merged() {
    // A superseded attempt is unmerged by definition; a safe delete would
    // refuse it and the retry could never reuse the name.
    let fx = Fixture::new().await;
    let node = fx.worktree("node", "al/run-1-node").await;
    write_and_commit(&node, "work.txt", "half\n", "partial work").await;
    remove_worktree(&fx.repo, &node).await.unwrap();

    assert!(git::branch_exists(&fx.repo, "al/run-1-node").await.unwrap());
    git::delete_branch(&fx.repo, "al/run-1-node").await.unwrap();
    assert!(!git::branch_exists(&fx.repo, "al/run-1-node").await.unwrap());
}

#[tokio::test]
async fn deleting_a_branch_that_is_checked_out_is_refused() {
    let fx = Fixture::new().await;
    fx.worktree("node", "al/run-1-node").await;

    let err = git::delete_branch(&fx.repo, "al/run-1-node")
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("al/run-1-node"), "{err}");
    assert!(git::branch_exists(&fx.repo, "al/run-1-node").await.unwrap());
}

#[tokio::test]
async fn deleting_a_branch_that_does_not_exist_is_an_error_not_a_silent_success() {
    let fx = Fixture::new().await;
    assert!(
        git::delete_branch(&fx.repo, "al/run-1-ghost")
            .await
            .is_err()
    );
}

/// Whether a bare repository carries `branch`, asked of the remote itself
/// rather than of the pushing side's tracking refs.
async fn remote_carries(origin: &Path, branch: &str) -> bool {
    git::run_allowing_failure(
        origin,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .await
    .unwrap()
    .succeeded()
}

#[tokio::test]
async fn a_repository_with_no_remote_says_so() {
    let fx = Fixture::new().await;
    assert!(!git::remote_exists(&fx.repo, "origin").await.unwrap());
}

#[tokio::test]
async fn a_configured_remote_is_visible() {
    let fx = Fixture::new().await;
    fx.with_origin().await;
    assert!(git::remote_exists(&fx.repo, "origin").await.unwrap());
    // Only the remote that was added — not any name at all.
    assert!(!git::remote_exists(&fx.repo, "upstream").await.unwrap());
}

#[tokio::test]
async fn pushing_a_branch_puts_it_on_the_remote() {
    let fx = Fixture::new().await;
    let origin = fx.with_origin().await;
    let node = fx.worktree("node", "al/run-1-node").await;
    write_and_commit(&node, "work.txt", "done\n", "node work").await;

    assert!(!remote_carries(&origin, "al/run-1-node").await);
    git::push_branch(&fx.repo, "origin", "al/run-1-node")
        .await
        .unwrap();
    assert!(remote_carries(&origin, "al/run-1-node").await);
}

/// A revise round appends a commit to a branch that was already published, so
/// the second push must fast-forward rather than be rejected.
#[tokio::test]
async fn pushing_a_branch_again_after_another_commit_fast_forwards() {
    let fx = Fixture::new().await;
    let origin = fx.with_origin().await;
    let node = fx.worktree("node", "al/run-1-node").await;

    write_and_commit(&node, "work.txt", "round one\n", "round 1").await;
    git::push_branch(&fx.repo, "origin", "al/run-1-node")
        .await
        .unwrap();

    let second = write_and_commit(&node, "work.txt", "round two\n", "round 2").await;
    git::push_branch(&fx.repo, "origin", "al/run-1-node")
        .await
        .unwrap();

    let on_remote = git::run_allowing_failure(&origin, &["rev-parse", "al/run-1-node"])
        .await
        .unwrap();
    assert_eq!(on_remote.stdout.trim(), second);
}

#[tokio::test]
async fn pushing_to_a_remote_that_does_not_exist_names_it() {
    let fx = Fixture::new().await;
    let node = fx.worktree("node", "al/run-1-node").await;
    write_and_commit(&node, "work.txt", "done\n", "node work").await;

    let err = git::push_branch(&fx.repo, "origin", "al/run-1-node")
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("origin"), "{err}");
}
