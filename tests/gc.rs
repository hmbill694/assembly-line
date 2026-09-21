use assembly_line::gc::{RepositoryLeftovers, StaleWorktree, reason_to_collect, remove};
use assembly_line::git;
use assembly_line::paths::{create_job, jobs_root};
use std::time::Duration;

mod support;

/// A repository whose job 1 has a state directory, plus a worktree directory
/// standing in for that job's checkout.
struct Fixture {
    _tmp: tempfile::TempDir,
    repo: std::path::PathBuf,
    worktree: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let worktree = tmp.path().join("wt/1");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        create_job(&jobs_root(&repo), 1).unwrap();

        Fixture {
            _tmp: tmp,
            repo,
            worktree,
        }
    }
}

/// The rule the whole module reduces to: a job's worktrees are wanted exactly
/// as long as the job is.
#[test]
fn a_job_whose_state_is_gone_is_collectable() {
    let fx = Fixture::new();
    std::fs::remove_dir_all(jobs_root(&fx.repo)).unwrap();

    let reason = reason_to_collect(Some(&fx.repo), 1, &fx.worktree, None).unwrap();
    assert!(reason.contains("no state directory"), "{reason}");
}

#[test]
fn worktrees_of_a_repository_that_is_gone_are_collectable() {
    let fx = Fixture::new();
    std::fs::remove_dir_all(&fx.repo).unwrap();

    let reason = reason_to_collect(Some(&fx.repo), 1, &fx.worktree, None).unwrap();
    assert!(reason.contains("no longer exists"), "{reason}");
}

/// Without the marker naming its repository there is no way to tell whether
/// the job still exists, so the leftovers are collectable by default.
#[test]
fn worktrees_with_no_known_repository_are_collectable() {
    let fx = Fixture::new();

    let reason = reason_to_collect(None, 1, &fx.worktree, None).unwrap();
    assert!(reason.contains("repository is unknown"), "{reason}");
}

/// Age never collects on its own — only when `--older-than` asked for it.
#[test]
fn age_collects_only_when_asked_for() {
    let fx = Fixture::new();

    assert_eq!(
        reason_to_collect(Some(&fx.repo), 1, &fx.worktree, None),
        None,
        "a live job's worktree must survive with no --older-than"
    );

    let reason = reason_to_collect(Some(&fx.repo), 1, &fx.worktree, Some(Duration::ZERO))
        .unwrap_or_default();
    assert!(
        reason.contains("untouched for"),
        "an idle threshold of zero should collect: {reason}"
    );
}

#[test]
fn a_worktree_younger_than_the_threshold_is_kept() {
    let fx = Fixture::new();

    assert_eq!(
        reason_to_collect(
            Some(&fx.repo),
            1,
            &fx.worktree,
            Some(Duration::from_hours(1))
        ),
        None
    );
}

#[tokio::test]
async fn removing_leftovers_deletes_them_and_counts_only_what_went() {
    let fx = Fixture::new();
    let never_existed = fx.worktree.parent().unwrap().join("2");

    let leftovers = RepositoryLeftovers {
        repo: None,
        stale: vec![
            StaleWorktree {
                path: never_existed.clone(),
                because: "its repository is unknown".to_string(),
            },
            StaleWorktree {
                path: fx.worktree.clone(),
                because: "its repository is unknown".to_string(),
            },
        ],
    };

    let removed = remove(&[leftovers]).await;

    assert!(
        !fx.worktree.exists(),
        "the directory that existed should be gone"
    );
    assert_eq!(removed.directories, 1, "only what actually went is counted");
    assert_eq!(removed.warnings.len(), 1, "{:?}", removed.warnings);
    assert!(
        removed.warnings[0].contains(&never_existed.display().to_string()),
        "the warning should name the directory: {}",
        removed.warnings[0]
    );
}

#[tokio::test]
async fn removing_a_worktree_also_drops_gits_record_of_it() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    support::init_git_repo(&repo).await;

    let worktree = tmp.path().join("wt/1/checkout");
    git::add_worktree(
        &repo,
        &worktree,
        "al/job-1",
        &git::WorktreeStart::CreatingBranch {
            at: "HEAD".to_string(),
        },
    )
    .await
    .unwrap();

    let leftovers = RepositoryLeftovers {
        repo: Some(repo.clone()),
        stale: vec![StaleWorktree {
            path: worktree.parent().unwrap().to_path_buf(),
            because: "job 1 has no state directory".to_string(),
        }],
    };

    let removed = remove(&[leftovers]).await;

    assert_eq!(removed.directories, 1);
    assert!(removed.warnings.is_empty(), "{:?}", removed.warnings);

    let listed = git::run_allowing_failure(&repo, &["worktree", "list"])
        .await
        .unwrap()
        .stdout;
    assert!(
        !listed.contains("al/job-1"),
        "git still lists the removed worktree: {listed}"
    );
}
