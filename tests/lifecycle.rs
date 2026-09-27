use assembly_line::config::Warning;
use assembly_line::git;
use assembly_line::lifecycle::{
    Note, Prepared, Refusal, RevisionRequest, StartRequest, prepare_revision, prepare_start,
};
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

    let prepared = prepare_start(&runner, &[], start_in(&h, Some("ghost"))).await;

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

    let prepared = prepare_start(&runner, &[], start_in(&h, None)).await;

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

    let prepared = prepare_start(&runner, &[], start_in(&h, None)).await;

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
    let runner = the_binary();

    let prepared = prepare_start(
        &runner,
        &[],
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
async fn revising_a_job_that_does_not_exist_cannot_be_prepared() {
    let h = Harness::new().await;
    let runner = the_binary();

    let prepared = prepare_revision(
        &runner,
        &[],
        RevisionRequest {
            job_id: 9,
            feedback: "more".into(),
            repo: Some(h.repo.clone()),
        },
    )
    .await;

    let refusal = refusal_of(prepared);
    assert!(refusal.to_string().contains("no such job: 9"), "{refusal}");
}
