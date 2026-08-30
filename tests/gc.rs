use assembly_line::gc::reason_to_collect;
use assembly_line::paths::{create_run, runs_root};
use std::time::Duration;

/// A repository whose run 1 has a state directory, plus a worktree directory
/// standing in for that run's checkouts.
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
        create_run(&runs_root(&repo), 1).unwrap();

        Fixture {
            _tmp: tmp,
            repo,
            worktree,
        }
    }
}

#[test]
fn a_live_runs_worktrees_are_kept_however_old() {
    let fx = Fixture::new();

    assert_eq!(
        reason_to_collect(Some(&fx.repo), 1, &fx.worktree, None),
        None
    );
}

/// The rule the whole module reduces to: a run's worktrees are wanted exactly
/// as long as the run is.
#[test]
fn a_run_whose_state_is_gone_is_collectable() {
    let fx = Fixture::new();
    std::fs::remove_dir_all(runs_root(&fx.repo)).unwrap();

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
/// the run still exists, so the leftovers are collectable by default.
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
        "a live run's worktree must survive with no --older-than"
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
