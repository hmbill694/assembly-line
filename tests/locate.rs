use assembly_line::event::{EventKind, EventLog};
use assembly_line::git;
use assembly_line::locate::{output_log_of, report_for_job};
use assembly_line::paths::{self, JobPaths, RepoKey};
use support::Harness;

mod support;

/// Job `id`'s directory under `h`'s root and the request that opens its
/// log, as the daemon leaves them when it queues the job.
async fn job_in(h: &Harness, id: u64) -> JobPaths {
    let jobs_dir = RepoKey::from_remote_url(h.origin.to_str().unwrap())
        .unwrap()
        .jobs_dir(&h.root);
    let job = paths::create_job(&jobs_dir, id.into()).unwrap();
    EventLog::open_append(job.events())
        .unwrap()
        .append(EventKind::RoundRequested {
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

    assert_eq!(report.id, 12);
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

    assert_eq!(report.id, 4);
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
