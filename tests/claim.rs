//! Claiming a job's id by creating its branch on the remote.

use assembly_line::admission::{Admitted, admit};
use assembly_line::claim::claim_job;
use assembly_line::git;
use assembly_line::job::JobId;
use support::Harness;

mod support;

/// An opted-in repository with `main` published to a bare origin, admitted
/// to run a job from `main`.
async fn admitted() -> (Harness, Admitted) {
    let h = Harness::new().await;
    let base = git::pinned(&h.repo, "origin", "main").await.unwrap();
    let admitted = admit(&h.repo, base, None).await.unwrap();
    (h, admitted)
}

#[tokio::test]
async fn the_first_claim_on_a_remote_is_job_1_at_the_base() {
    let (h, admitted) = admitted().await;

    let id = claim_job(&h.repo, "origin", &admitted).await.unwrap();

    assert_eq!(id, JobId::from(1));
    assert_eq!(
        git::sha_at_ref(&h.origin, "al/job-1").await.unwrap(),
        admitted.base().sha
    );
}

#[tokio::test]
async fn a_second_claim_from_the_same_base_gets_the_next_id() {
    let (h, admitted) = admitted().await;
    assert_eq!(
        claim_job(&h.repo, "origin", &admitted).await.unwrap(),
        JobId::from(1)
    );

    assert_eq!(
        claim_job(&h.repo, "origin", &admitted).await.unwrap(),
        JobId::from(2)
    );
}

#[tokio::test]
async fn claims_racing_on_one_remote_all_get_different_ids() {
    let (h, admitted) = admitted().await;

    let claims = join_all((0..6).map(|_| {
        let (repo, admitted) = (h.repo.clone(), admitted.clone());
        tokio::spawn(async move { claim_job(&repo, "origin", &admitted).await.unwrap() })
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
    let (h, admitted) = admitted().await;
    git::push_head_as(&h.repo, "origin", "al/job-41")
        .await
        .unwrap();

    assert_eq!(
        claim_job(&h.repo, "origin", &admitted).await.unwrap(),
        JobId::from(42)
    );
}

#[tokio::test]
async fn a_branch_at_the_last_id_is_named_in_the_refusal() {
    let (h, admitted) = admitted().await;
    let last = JobId::from(u64::MAX).branch_name();
    git::push_head_as(&h.repo, "origin", &last).await.unwrap();

    let err = claim_job(&h.repo, "origin", &admitted).await.unwrap_err();
    assert!(err.to_string().contains(&last), "{err}");
}

#[tokio::test]
async fn an_unreachable_remote_is_an_error_not_a_lost_race() {
    let (h, admitted) = admitted().await;
    std::fs::remove_dir_all(&h.origin).unwrap();

    assert!(claim_job(&h.repo, "origin", &admitted).await.is_err());
}
