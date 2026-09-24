use assembly_line::git::{
    self, DiffStat, check_out_new_branch, clone_into, commit_all, commit_all_except,
    commit_as_assembly_line, diff_stat_against, head_sha, is_dirty,
};
use std::path::{Path, PathBuf};

mod support;

/// A repository with one commit, plus a sibling directory for scratch clones.
struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    clones: PathBuf,
}

impl Fixture {
    async fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let clones = tmp.path().join("clones");
        std::fs::create_dir_all(&clones).unwrap();
        support::init_git_repo(&repo).await;

        Fixture {
            _tmp: tmp,
            repo,
            clones,
        }
    }

    async fn base(&self) -> String {
        head_sha(&self.repo).await.unwrap()
    }

    async fn with_origin(&self) -> PathBuf {
        let origin = self.repo.parent().expect("a parent").join("origin.git");
        support::add_origin(&self.repo, &origin).await;
        origin
    }

    /// Clone the repo itself and check `branch` out at its current HEAD — a
    /// plain clone standing in for the scratch checkout a real job makes.
    async fn clone_on_branch(&self, name: &str, branch: &str) -> PathBuf {
        let base = self.base().await;
        clone_checked_out(&self.repo, &self.clones, name, branch, &base).await
    }
}

/// Clone `source` into a fresh directory under `into`, and check `branch` out
/// at `at`. The same two calls `workspace::create` makes to build a job's
/// scratch checkout.
async fn clone_checked_out(
    source: &Path,
    into: &Path,
    name: &str,
    branch: &str,
    at: &str,
) -> PathBuf {
    let path = into.join(name);
    std::fs::create_dir_all(&path).unwrap();
    clone_into(source.to_str().unwrap(), &path).await.unwrap();
    check_out_new_branch(&path, branch, at).await.unwrap();
    commit_as_assembly_line(&path).await.unwrap();
    path
}

async fn write_and_commit(clone: &Path, name: &str, body: &str, message: &str) -> String {
    std::fs::write(clone.join(name), body).unwrap();
    commit_all(clone, message)
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
async fn is_dirty_tracks_uncommitted_work() {
    let fx = Fixture::new().await;
    assert!(!is_dirty(&fx.repo).await.unwrap());

    std::fs::write(fx.repo.join("scratch.txt"), "wip").unwrap();
    assert!(is_dirty(&fx.repo).await.unwrap());
}

#[tokio::test]
async fn diff_stat_counts_files_and_lines_against_a_base() {
    let fx = Fixture::new().await;
    let base = fx.base().await;
    let node = fx.clone_on_branch("stat", "al/job-2").await;

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
async fn diff_stat_of_an_unchanged_clone_is_empty() {
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
    let node = fx.clone_on_branch("secret", "al/job-3").await;

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
    let node = fx.clone_on_branch("only-secret", "al/job-4").await;

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
async fn a_secret_committed_by_the_agent_fails_the_job() {
    let fx = Fixture::new().await;
    let node = fx.clone_on_branch("agent-commit", "al/job-5").await;

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
}

#[tokio::test]
async fn a_failed_git_command_carries_gits_own_message() {
    let fx = Fixture::new().await;

    // A ref git itself rejects, so the error is git's stderr rather than a
    // spawn failure — which would never reach `stdout_or_error` at all.
    let err = git::sha_at_ref(&fx.repo, "no-such-ref")
        .await
        .unwrap_err()
        .to_string();

    assert!(err.contains("no-such-ref"), "{err}");
    assert!(
        err.contains("unknown revision") || err.contains("Needed a single revision"),
        "not git's own message: {err}"
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

/// A push happens from a job's own clone, not from the repository it was cut
/// from — so this pushes from a clone of `origin`, exactly as
/// `workspace::create` builds one.
#[tokio::test]
async fn pushing_a_branch_puts_it_on_the_remote() {
    let fx = Fixture::new().await;
    let origin = fx.with_origin().await;
    support::publish_main(&fx.repo).await;
    let base = fx.base().await;
    let node = clone_checked_out(&origin, &fx.clones, "node", "al/job-1", &base).await;
    write_and_commit(&node, "work.txt", "done\n", "node work").await;

    assert!(!remote_carries(&origin, "al/job-1").await);
    git::push_branch(&node, "origin", "al/job-1").await.unwrap();
    assert!(remote_carries(&origin, "al/job-1").await);
}

/// A revise round appends a commit to a branch that was already published, so
/// the second push must fast-forward rather than be rejected.
#[tokio::test]
async fn pushing_a_branch_again_after_another_commit_fast_forwards() {
    let fx = Fixture::new().await;
    let origin = fx.with_origin().await;
    support::publish_main(&fx.repo).await;
    let base = fx.base().await;
    let node = clone_checked_out(&origin, &fx.clones, "node", "al/job-1", &base).await;

    write_and_commit(&node, "work.txt", "round one\n", "round 1").await;
    git::push_branch(&node, "origin", "al/job-1").await.unwrap();

    let second = write_and_commit(&node, "work.txt", "round two\n", "round 2").await;
    git::push_branch(&node, "origin", "al/job-1").await.unwrap();

    let on_remote = git::run_allowing_failure(&origin, &["rev-parse", "al/job-1"])
        .await
        .unwrap();
    assert_eq!(on_remote.stdout.trim(), second);
}

#[tokio::test]
async fn pushing_to_a_remote_that_does_not_exist_names_it() {
    let fx = Fixture::new().await;
    let base = fx.base().await;
    check_out_new_branch(&fx.repo, "al/job-1", &base)
        .await
        .unwrap();

    let err = git::push_branch(&fx.repo, "origin", "al/job-1")
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("origin"), "{err}");
}

/// Configuration is read from a ref, never from a checkout — which is what
/// stops a job editing the settings that govern it.
#[tokio::test]
async fn a_file_is_read_from_a_ref_not_the_working_tree() {
    let fx = Fixture::new().await;
    std::fs::write(fx.repo.join("marker"), "committed\n").unwrap();
    commit_all(&fx.repo, "add marker").await.unwrap().unwrap();

    // The working tree disagrees with HEAD.
    std::fs::write(fx.repo.join("marker"), "edited\n").unwrap();

    let at_head = git::file_at_ref(&fx.repo, "HEAD", "marker").await.unwrap();
    assert_eq!(at_head.as_deref(), Some("committed\n"));

    let absent = git::file_at_ref(&fx.repo, "HEAD", "nope").await.unwrap();
    assert_eq!(absent, None, "a missing path is None, not an error");
}

#[tokio::test]
async fn a_ref_resolves_to_the_commit_it_names() {
    let fx = Fixture::new().await;

    assert_eq!(
        git::sha_at_ref(&fx.repo, "main").await.unwrap(),
        head_sha(&fx.repo).await.unwrap()
    );
    assert!(git::sha_at_ref(&fx.repo, "no-such-ref").await.is_err());
}

#[tokio::test]
async fn a_remotes_url_is_read_back_and_a_missing_remote_is_none() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    support::init_git_repo(&repo).await;
    assert_eq!(git::remote_url(&repo, "origin").await.unwrap(), None);

    let origin = tmp.path().join("origin.git");
    support::add_origin(&repo, &origin).await;
    assert_eq!(
        git::remote_url(&repo, "origin").await.unwrap().as_deref(),
        Some(origin.to_str().unwrap())
    );
}

/// Git reads a relative remote path from the repository; a job's clone runs
/// from a scratch directory, where the same path names nothing.
#[tokio::test]
async fn a_relative_remote_path_is_resolved_against_the_repository() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    support::init_git_repo(&repo).await;
    git::run_allowing_failure(&repo, &["remote", "add", "origin", "../origin.git"])
        .await
        .unwrap();

    assert_eq!(
        git::remote_url(&repo, "origin").await.unwrap(),
        Some(repo.join("../origin.git").to_string_lossy().into_owned())
    );
}

#[tokio::test]
async fn remote_urls_that_name_a_host_are_left_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    support::init_git_repo(&repo).await;

    for url in [
        "git@example.com:team/repo.git",
        "https://example.com/team/repo.git",
    ] {
        git::run_allowing_failure(&repo, &["remote", "add", "origin", url])
            .await
            .unwrap();
        assert_eq!(
            git::remote_url(&repo, "origin").await.unwrap().as_deref(),
            Some(url)
        );
        git::run_allowing_failure(&repo, &["remote", "remove", "origin"])
            .await
            .unwrap();
    }
}

/// A job's clone pushes back to where it cloned from, so a remote that
/// pushes elsewhere would send the job's branch to the wrong place.
#[tokio::test]
async fn a_remote_that_pushes_somewhere_else_is_refused_with_what_to_change() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    support::init_git_repo(&repo).await;
    support::add_origin(&repo, &tmp.path().join("origin.git")).await;
    git::run_allowing_failure(
        &repo,
        &["remote", "set-url", "--push", "origin", "/elsewhere.git"],
    )
    .await
    .unwrap();

    let err = git::remote_url(&repo, "origin")
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("remote.origin.pushurl"), "{err}");
}

/// A push rewrite in the repository's own config is as invisible to the
/// clone as a pushurl, and the refusal names the setting that caused it.
#[tokio::test]
async fn a_push_rewrite_only_this_repository_holds_is_refused_by_name() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let origin = tmp.path().join("origin.git");
    support::init_git_repo(&repo).await;
    support::add_origin(&repo, &origin).await;
    let rewrite_key = format!(
        "url.{}.pushInsteadOf",
        tmp.path().join("elsewhere.git").display()
    );
    git::run_allowing_failure(&repo, &["config", &rewrite_key, origin.to_str().unwrap()])
        .await
        .unwrap();

    let err = git::remote_url(&repo, "origin")
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("pushinsteadof"), "{err}");
    assert!(err.contains("global config"), "{err}");
}

/// A job starts from what the remote says a ref is, not what the local
/// repository says: unpushed work is not something a clone can see.
#[tokio::test]
async fn a_ref_is_pinned_to_the_commit_the_remote_has_for_it() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    support::init_git_repo(&repo).await;
    support::add_origin(&repo, &tmp.path().join("origin.git")).await;
    support::publish_main(&repo).await;
    let pushed = head_sha(&repo).await.unwrap();

    std::fs::write(repo.join("unpushed.txt"), "local only\n").unwrap();
    commit_all(&repo, "unpushed").await.unwrap().unwrap();

    let pinned = git::pinned(&repo, "origin", "main").await.unwrap();
    assert_eq!(
        pinned,
        git::PinnedRef {
            name: "main".into(),
            sha: pushed
        }
    );
}

#[tokio::test]
async fn a_ref_the_remote_does_not_have_cannot_be_pinned() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    support::init_git_repo(&repo).await;
    support::add_origin(&repo, &tmp.path().join("origin.git")).await;
    support::publish_main(&repo).await;

    let err = git::pinned(&repo, "origin", "never-pushed")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("never-pushed"), "{err}");
    assert!(err.to_string().contains("push it first"), "{err}");
}

/// Pushing cannot fix a remote that does not answer, so a fetch that failed
/// for any reason other than the ref being absent keeps git's own complaint.
#[tokio::test]
async fn a_remote_that_cannot_be_reached_is_not_told_to_push_first() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    support::init_git_repo(&repo).await;
    let nowhere = tmp.path().join("no-such-origin.git");
    let added = git::run_allowing_failure(
        &repo,
        &["remote", "add", "origin", nowhere.to_str().unwrap()],
    )
    .await
    .unwrap();
    assert!(added.succeeded(), "{}", added.stderr);

    let err = git::pinned(&repo, "origin", "main")
        .await
        .unwrap_err()
        .to_string();
    assert!(!err.contains("push it first"), "{err}");
    assert!(err.contains("fetch origin main"), "{err}");
}

#[tokio::test]
async fn a_clone_checks_out_a_new_branch_at_the_commit_it_is_given() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    support::init_git_repo(&repo).await;
    let origin = tmp.path().join("origin.git");
    support::add_origin(&repo, &origin).await;
    support::publish_main(&repo).await;
    let at = head_sha(&repo).await.unwrap();

    let clone = tmp.path().join("clone");
    std::fs::create_dir_all(&clone).unwrap();
    git::clone_into(origin.to_str().unwrap(), &clone)
        .await
        .unwrap();
    git::check_out_new_branch(&clone, "al/job-1", &at)
        .await
        .unwrap();

    assert_eq!(head_sha(&clone).await.unwrap(), at);
    assert_eq!(
        git::current_branch(&clone).await.unwrap().as_deref(),
        Some("al/job-1")
    );
    assert!(clone.join("README.md").is_file());
}
