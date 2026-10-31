use assembly_line::event::EventKind;
use assembly_line::git;
use assembly_line::job::JobId;
use assembly_line::locate::{NamedRepo, output_log_of, report_for_job};
use assembly_line::paths::{JobPaths, JobPlace};
use assembly_line::remote::Remote;
use std::path::Path;
use support::Harness;

mod support;

/// Job `id`'s directory under `h`'s root and the request that opens its
/// log, as the daemon leaves them when it queues the job.
async fn job_in(h: &Harness, id: u64) -> JobPaths {
    let job = JobPlace {
        key: Remote::from_url(h.origin.to_str().unwrap())
            .repo_key()
            .unwrap(),
        id: id.into(),
    }
    .create(&h.root)
    .unwrap();
    job.append(EventKind::RoundRequested {
        remote_url: h.origin.to_string_lossy().into_owned(),
        base: git::PinnedRef {
            name: "main".into(),
            sha: git::sha_at_ref(&h.origin, "main").await.unwrap(),
        },
        prompt: "x".into(),
        provider: "fake".into(),
    })
    .unwrap();
    job
}

fn checkout(h: &Harness) -> String {
    h.repo.to_string_lossy().into_owned()
}

#[tokio::test]
async fn status_without_a_job_id_reports_the_latest_job() {
    let h = Harness::new().await;
    job_in(&h, 3).await;
    job_in(&h, 12).await;

    let report = report_for_job(&h.root, None, Some(checkout(&h)))
        .await
        .unwrap();

    assert_eq!(report.id, JobId::from(12));
}

/// `--repo` takes the remote's URL as readily as a checkout of it, as
/// `submit` does.
#[tokio::test]
async fn a_job_is_found_by_its_remotes_url_too() {
    let h = Harness::new().await;
    job_in(&h, 4).await;

    let report = report_for_job(&h.root, None, Some(h.origin.to_string_lossy().into_owned()))
        .await
        .unwrap();

    assert_eq!(report.id, JobId::from(4));
}

#[tokio::test]
async fn a_named_checkout_is_read_for_its_remote_and_its_branch() {
    let h = Harness::new().await;

    let repo = NamedRepo::named_or_enclosing(Some(checkout(&h)))
        .await
        .unwrap();

    assert_eq!(Path::new(repo.remote.url()), h.origin);
    assert_eq!(repo.checkout.as_deref(), Some(h.repo.as_path()));
    assert_eq!(repo.ref_to_start_from(None).await.unwrap(), "main");
    assert_eq!(
        repo.ref_to_start_from(Some("release".into()))
            .await
            .unwrap(),
        "release"
    );
}

#[tokio::test]
async fn a_named_url_is_the_remote_itself_and_needs_a_ref() {
    let h = Harness::new().await;
    let url = h.origin.to_string_lossy().into_owned();

    let repo = NamedRepo::named_or_enclosing(Some(url.clone()))
        .await
        .unwrap();

    assert_eq!(repo.remote.url(), url);
    assert_eq!(repo.checkout, None);
    assert_eq!(
        repo.ref_to_start_from(None).await.unwrap_err().to_string(),
        "a remote URL has no checked-out branch — name the ref to start from with --ref"
    );
    assert_eq!(
        repo.ref_to_start_from(Some("main".into())).await.unwrap(),
        "main"
    );
}

#[tokio::test]
async fn a_checkout_with_a_detached_head_needs_a_ref() {
    let h = Harness::new().await;
    git::run_allowing_failure(&h.repo, &["checkout", "--quiet", "--detach"])
        .await
        .unwrap();

    let repo = NamedRepo::named_or_enclosing(Some(checkout(&h)))
        .await
        .unwrap();

    assert_eq!(
        repo.ref_to_start_from(None).await.unwrap_err().to_string(),
        "HEAD is detached — name the ref to start from with --ref"
    );
}

#[tokio::test]
async fn status_in_a_repository_with_no_jobs_says_so() {
    let h = Harness::new().await;

    let err = report_for_job(&h.root, None, Some(checkout(&h)))
        .await
        .unwrap_err();

    assert_eq!(err.to_string(), "no jobs yet");
}

#[tokio::test]
async fn a_job_that_has_captured_nothing_has_no_log_to_show() {
    let h = Harness::new().await;
    job_in(&h, 1).await;

    let err = output_log_of(&h.root, 1, Some(checkout(&h)))
        .await
        .unwrap_err();

    assert_eq!(err.to_string(), "job 1 has captured no output yet");
}

#[tokio::test]
async fn a_job_that_has_captured_output_names_its_log() {
    let h = Harness::new().await;
    let job = job_in(&h, 1).await;
    std::fs::write(job.log(), "agent says hi\n").unwrap();

    assert_eq!(
        output_log_of(&h.root, 1, Some(checkout(&h))).await.unwrap(),
        job.log()
    );
}
