use assembly_line::config::Warning;
use assembly_line::event::{EventKind, EventLog};
use assembly_line::git;
use assembly_line::lifecycle::{
    Note, Prepared, Refusal, RevisionRequest, StartRequest, output_log_of, prepare_revision,
    prepare_start, report_for_job,
};
use assembly_line::locate;
use assembly_line::paths::{self, JobPaths, RepoKey};
use assembly_line::runner::local::LocalRunner;
use support::{Harness, commit_all, config_running};

mod support;

fn the_binary() -> LocalRunner {
    LocalRunner::using(env!("CARGO_BIN_EXE_assembly"))
}

fn start_in(h: &Harness, provider: Option<&str>) -> StartRequest {
    StartRequest {
        prompt: Some("x".into()),
        prompt_file: None,
        repo: Some(h.repo.clone()),
        base_ref: None,
        provider: provider.map(str::to_string),
    }
}

/// Job `id`'s directory under `h`'s root and the request that opens its
/// log, as `run` would have left them before its round started.
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

fn refusal_of<R>(prepared: Prepared<'_, R>) -> Refusal {
    let Err(refusal) = prepared.round else {
        panic!("the round was ready, not refused");
    };
    refusal
}

#[tokio::test]
async fn a_refused_round_still_carries_its_config_warnings() {
    let h = Harness::new().await;
    let runner = the_binary();

    let prepared = prepare_start(&runner, &[], &h.root, start_in(&h, Some("ghost"))).await;

    assert_eq!(prepared.notes, [Note::ConfigWarning(Warning::NoVerify)]);
}

#[tokio::test]
async fn a_new_job_is_ready_without_an_announcement() {
    let h = Harness::with_config(&format!(
        "verify = \"true\"\n{}",
        config_running("fake-agent.sh")
    ))
    .await;
    let runner = the_binary();

    let prepared = prepare_start(&runner, &[], &h.root, start_in(&h, None)).await;

    assert!(prepared.notes.is_empty(), "{:?}", prepared.notes);
    let Ok(ready) = prepared.round else {
        panic!("a runnable repository was refused");
    };
    assert_eq!(ready.to_announcement_line(), None);
}

#[tokio::test]
async fn unpushed_local_work_is_noted_against_the_remotes_commit() {
    let h = Harness::new().await;
    let remotes = git::sha_at_ref(&h.origin, "main").await.unwrap();
    std::fs::write(h.repo.join("unpushed.txt"), "mine\n").unwrap();
    commit_all(&h.repo, "unpushed").await.unwrap().unwrap();
    let runner = the_binary();

    let prepared = prepare_start(&runner, &[], &h.root, start_in(&h, None)).await;

    assert!(
        prepared.notes.contains(&Note::LocalRefDiffers {
            base_ref: "main".into(),
            remote: "origin".into(),
            start_sha: remotes,
        }),
        "{:?}",
        prepared.notes
    );
    assert!(prepared.round.is_ok());
}

#[tokio::test]
async fn a_repository_with_no_remote_cannot_be_prepared() {
    let repo = support::repo_with_initial_commit().await;
    let root = tempfile::tempdir().unwrap();
    let runner = the_binary();

    let prepared = prepare_start(
        &runner,
        &[],
        root.path(),
        StartRequest {
            prompt: Some("x".into()),
            prompt_file: None,
            repo: Some(repo.path().to_path_buf()),
            base_ref: None,
            provider: None,
        },
    )
    .await;

    let refusal = refusal_of(prepared);
    assert!(matches!(refusal, Refusal::Unpreparable(_)));
    assert!(
        refusal.to_string().contains("no 'origin' remote"),
        "{refusal}"
    );
    assert!(refusal.itemized_reasons().is_empty());
}

#[tokio::test]
async fn status_without_a_job_id_reports_the_latest_job() {
    let h = Harness::new().await;
    job_in(&h, 3).await;
    job_in(&h, 12).await;

    let report = report_for_job(&h.root, None, Some(h.repo.clone()))
        .await
        .unwrap();

    assert_eq!(report.id, 12);
}

#[tokio::test]
async fn status_in_a_repository_with_no_jobs_says_so() {
    let h = Harness::new().await;

    let err = report_for_job(&h.root, None, Some(h.repo.clone()))
        .await
        .unwrap_err();

    assert_eq!(err.to_string(), "no jobs yet");
}

#[tokio::test]
async fn a_job_that_has_captured_nothing_has_no_log_to_show() {
    let h = Harness::new().await;
    job_in(&h, 1).await;

    let err = output_log_of(&h.root, 1, Some(h.repo.clone()))
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
        output_log_of(&h.root, 1, Some(h.repo.clone()))
            .await
            .unwrap(),
        job.log()
    );
}

#[tokio::test]
async fn revising_a_job_that_does_not_exist_cannot_be_prepared() {
    let h = Harness::new().await;
    let runner = the_binary();

    let prepared = prepare_revision(
        &runner,
        &[],
        &h.root,
        RevisionRequest {
            job_id: 9,
            prompt: Some("more".into()),
            prompt_file: None,
            repo: Some(h.repo.clone()),
        },
    )
    .await;

    let refusal = refusal_of(prepared);
    assert!(refusal.to_string().contains("no such job: 9"), "{refusal}");
}

/// Counting the `RoundStarted` lines that survive a torn log would hand the
/// next round a number the job already used.
#[tokio::test]
async fn a_revise_is_numbered_past_the_highest_round_recorded() {
    let h = Harness::new().await;
    let first = h.run_job("x").await;
    assert!(first.passed);
    let job = locate::job_at(&h.root, Some(h.repo.clone()), Some(first.job_id.into()))
        .await
        .unwrap();
    let mut log = EventLog::open_append(job.events()).unwrap();
    log.append(EventKind::RoundStarted { round: 3 }).unwrap();
    log.append(EventKind::RoundPassed).unwrap();
    let runner = the_binary();

    let prepared = prepare_revision(
        &runner,
        &[],
        &h.root,
        RevisionRequest {
            job_id: first.job_id.into(),
            prompt: Some("more".into()),
            prompt_file: None,
            repo: Some(h.repo.clone()),
        },
    )
    .await;

    let Ok(ready) = prepared.round else {
        panic!("the revise was refused");
    };
    assert_eq!(
        ready.to_announcement_line().as_deref(),
        Some("revising job 1 (round 4)")
    );
}

#[tokio::test]
async fn every_round_asked_for_is_recorded_before_it_starts() {
    let h = Harness::new().await;
    let first = h.run_job("add auth").await;
    let second = h.revise_job(first.job_id, "use sessions").await;

    let base = git::pinned(&h.repo, "origin", "main").await.unwrap();
    let requests: Vec<(usize, &str)> = second
        .events
        .iter()
        .enumerate()
        .filter_map(|(at, kind)| match kind {
            EventKind::RoundRequested {
                prompt,
                base: asked,
                ..
            } => {
                // A revise starts from the job's branch, but asks for its base.
                assert_eq!(asked, &base, "{kind:?}");
                Some((at, prompt.as_str()))
            }
            _ => None,
        })
        .collect();
    let starts: Vec<usize> = second
        .events
        .iter()
        .enumerate()
        .filter_map(|(at, kind)| matches!(kind, EventKind::RoundStarted { .. }).then_some(at))
        .collect();

    assert_eq!(requests.len(), 2, "{:?}", second.events);
    assert_eq!(requests[0].1, "add auth");
    assert!(requests[1].1.contains("use sessions"), "{}", requests[1].1);
    assert!(
        requests
            .iter()
            .zip(&starts)
            .all(|((requested_at, _), started_at)| requested_at < started_at),
        "{:?}",
        second.events
    );
}

/// A job directory whose log holds no request — allocated, but its first
/// round never recorded — has nothing to revise it from.
#[tokio::test]
async fn a_job_with_no_recorded_request_cannot_be_revised() {
    let h = Harness::new().await;
    let jobs_dir = RepoKey::from_remote_url(h.origin.to_str().unwrap())
        .unwrap()
        .jobs_dir(&h.root);
    paths::create_job(&jobs_dir, 1.into()).unwrap();
    let runner = the_binary();

    let prepared = prepare_revision(
        &runner,
        &[],
        &h.root,
        RevisionRequest {
            job_id: 1,
            prompt: Some("more".into()),
            prompt_file: None,
            repo: Some(h.repo.clone()),
        },
    )
    .await;

    let refusal = refusal_of(prepared);
    assert!(
        refusal.to_string().contains("no recorded request"),
        "{refusal}"
    );
}
