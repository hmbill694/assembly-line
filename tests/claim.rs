//! Claiming a job's id by creating its branch on the remote.

use assembly_line::claim::claim_job;
use assembly_line::git;
use assembly_line::job::JobId;

mod support;

/// A repository with `main` published to a bare origin, and its sha.
async fn published() -> (tempfile::TempDir, std::path::PathBuf, String) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    support::init_git_repo(&repo).await;
    support::add_origin(&repo, &tmp.path().join("origin.git")).await;
    support::publish_main(&repo).await;
    let sha = git::head_sha(&repo).await.unwrap();
    (tmp, repo, sha)
}

#[tokio::test]
async fn the_first_claim_on_a_remote_is_job_1_at_the_base() {
    let (tmp, repo, sha) = published().await;

    let id = claim_job(&repo, "origin", &sha).await.unwrap();

    assert_eq!(id, JobId::from(1));
    let origin = tmp.path().join("origin.git");
    assert_eq!(git::sha_at_ref(&origin, "al/job-1").await.unwrap(), sha);
}

#[tokio::test]
async fn a_second_claim_from_the_same_base_gets_the_next_id() {
    let (_tmp, repo, sha) = published().await;
    assert_eq!(
        claim_job(&repo, "origin", &sha).await.unwrap(),
        JobId::from(1)
    );

    assert_eq!(
        claim_job(&repo, "origin", &sha).await.unwrap(),
        JobId::from(2)
    );
}

#[tokio::test]
async fn claims_racing_on_one_remote_all_get_different_ids() {
    let (_tmp, repo, sha) = published().await;

    let claims = join_all((0..6).map(|_| {
        let (repo, sha) = (repo.clone(), sha.clone());
        tokio::spawn(async move { claim_job(&repo, "origin", &sha).await.unwrap() })
    }))
    .await;

    let mut ids: Vec<u64> = claims.into_iter().map(u64::from).collect();
    ids.sort_unstable();
    assert_eq!(ids, [1, 2, 3, 4, 5, 6]);
}

/// Awaits every handle in order. `futures` is not a dependency, and six
/// handles need no more than this.
async fn join_all(handles: impl IntoIterator<Item = tokio::task::JoinHandle<JobId>>) -> Vec<JobId> {
    let mut ids = Vec::new();
    for handle in handles {
        ids.push(handle.await.unwrap());
    }
    ids
}

#[tokio::test]
async fn somebody_elses_higher_job_branch_is_claimed_past() {
    let (_tmp, repo, sha) = published().await;
    git::push_head_as(&repo, "origin", "al/job-41")
        .await
        .unwrap();

    assert_eq!(
        claim_job(&repo, "origin", &sha).await.unwrap(),
        JobId::from(42)
    );
}

#[tokio::test]
async fn a_branch_at_the_last_id_is_named_in_the_refusal() {
    let (_tmp, repo, sha) = published().await;
    let last = JobId::from(u64::MAX).branch_name();
    git::push_head_as(&repo, "origin", &last).await.unwrap();

    let err = claim_job(&repo, "origin", &sha).await.unwrap_err();
    assert!(err.to_string().contains(&last), "{err}");
}

#[tokio::test]
async fn an_unreachable_remote_is_an_error_not_a_lost_race() {
    let (tmp, repo, sha) = published().await;
    std::fs::remove_dir_all(tmp.path().join("origin.git")).unwrap();

    assert!(claim_job(&repo, "origin", &sha).await.is_err());
}
