use assembly_line::git;
use assembly_line::submission::{LocalRefDiffers, SubmitRequest, prepare_submission};
use support::{Harness, commit_all};

mod support;

fn submitting(h: &Harness) -> SubmitRequest {
    SubmitRequest {
        prompt: Some("x".into()),
        repo: Some(h.repo.to_string_lossy().into_owned()),
        ..SubmitRequest::default()
    }
}

#[tokio::test]
async fn a_new_job_starts_from_the_checked_out_branch_of_the_checkouts_remote() {
    let h = Harness::new().await;

    let prepared = prepare_submission(submitting(&h)).await.unwrap();

    assert_eq!(prepared.submission.remote_url, h.origin.to_str().unwrap());
    assert_eq!(prepared.submission.base_ref.as_deref(), Some("main"));
    assert_eq!(prepared.submission.prompt, "x");
    assert!(prepared.notes.is_empty(), "{:?}", prepared.notes);
}

#[tokio::test]
async fn unpushed_local_work_is_noted_against_the_remotes_commit() {
    let h = Harness::new().await;
    let remotes = git::sha_at_ref(&h.origin, "main").await.unwrap();
    std::fs::write(h.repo.join("unpushed.txt"), "mine\n").unwrap();
    commit_all(&h.repo, "unpushed").await.unwrap().unwrap();

    let prepared = prepare_submission(submitting(&h)).await.unwrap();

    assert_eq!(
        prepared.notes,
        [LocalRefDiffers {
            base_ref: "main".into(),
            remote: "origin".into(),
            remote_sha: remotes
        }]
    );
    assert_eq!(prepared.submission.base_ref.as_deref(), Some("main"));
}

#[tokio::test]
async fn a_repository_with_no_remote_cannot_be_submitted() {
    let repo = support::repo_with_initial_commit().await;

    let err = prepare_submission(SubmitRequest {
        prompt: Some("x".into()),
        repo: Some(repo.path().to_string_lossy().into_owned()),
        ..SubmitRequest::default()
    })
    .await
    .unwrap_err();

    assert!(err.to_string().contains("no 'origin' remote"), "{err}");
}

#[tokio::test]
async fn a_revise_names_no_ref_and_leaves_the_base_to_the_job() {
    let h = Harness::new().await;

    let prepared = prepare_submission(SubmitRequest {
        job: Some(3),
        ..submitting(&h)
    })
    .await
    .unwrap();

    assert_eq!(prepared.submission.base_ref, None);
    assert_eq!(prepared.submission.job, Some(3));
    assert!(prepared.notes.is_empty());
}

#[tokio::test]
async fn a_remote_url_is_submitted_as_it_is_and_needs_a_ref() {
    let url = "https://example.invalid/o/r.git";
    let named = |base_ref: Option<&str>| SubmitRequest {
        prompt: Some("x".into()),
        repo: Some(url.into()),
        base_ref: base_ref.map(str::to_string),
        ..SubmitRequest::default()
    };

    let err = prepare_submission(named(None)).await.unwrap_err();
    assert!(err.to_string().contains("--ref"), "{err}");

    let prepared = prepare_submission(named(Some("main"))).await.unwrap();
    assert_eq!(prepared.submission.remote_url, url);
    assert_eq!(prepared.submission.base_ref.as_deref(), Some("main"));
}
