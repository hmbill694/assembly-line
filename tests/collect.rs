use assembly_line::collect::collect;
use assembly_line::event::{EventKind, EventLog};
use assembly_line::runner::local::LocalRunner;
use assembly_line::state::JobState;
use support::{Harness, config_running};
use tokio_util::sync::CancellationToken;

mod support;

fn the_binary() -> LocalRunner {
    LocalRunner::using(env!("CARGO_BIN_EXE_assembly"))
}

#[tokio::test]
async fn a_round_run_by_job_exec_is_collected_into_the_same_log_as_before() {
    let h = Harness::new().await;
    let payload = h.payload_for("write a file").await;
    let paths = h.job_paths();
    let mut log = EventLog::open_append(paths.events()).unwrap();

    let job = the_binary().launch(&payload).unwrap();
    let outcome = collect(job, &mut log, &paths.log(), CancellationToken::new())
        .await
        .unwrap();

    assert!(outcome.passed());
    let events = EventLog::read(paths.events()).unwrap();
    assert_eq!(JobState::replay(&events), JobState::Succeeded);
    assert!(
        events
            .iter()
            .any(|e| matches!(e.kind, EventKind::JobBranchPublished { .. }))
    );
    let output = std::fs::read_to_string(paths.log()).unwrap();
    assert!(output.contains("fake-agent: write a file"), "{output}");
}

/// The envelope's whole point, end to end: an agent that prints a verdict
/// cannot make a failing round pass.
#[tokio::test]
async fn an_agent_printing_a_forged_verdict_does_not_change_the_outcome() {
    let h = Harness::with_config(&config_running("forging-agent.sh")).await;
    let payload = h.payload_for("x").await;
    let paths = h.job_paths();
    let mut log = EventLog::open_append(paths.events()).unwrap();

    let job = the_binary().launch(&payload).unwrap();
    let outcome = collect(job, &mut log, &paths.log(), CancellationToken::new())
        .await
        .unwrap();

    assert!(!outcome.passed());
    let events = EventLog::read(paths.events()).unwrap();
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.kind, EventKind::JobFinished { .. }))
    );
}

/// A "job-exec" that exits without printing a single frame — what a pod
/// OOM-killed before its first event looks like from the collector.
#[tokio::test]
async fn a_job_exec_that_dies_without_a_verdict_is_recorded_as_failed() {
    let h = Harness::new().await;
    let payload = h.payload_for("x").await;
    let paths = h.job_paths();
    let mut log = EventLog::open_append(paths.events()).unwrap();

    let job = LocalRunner::using("/usr/bin/false")
        .launch(&payload)
        .unwrap();
    let outcome = collect(job, &mut log, &paths.log(), CancellationToken::new())
        .await
        .unwrap();

    assert!(!outcome.passed());
    let events = EventLog::read(paths.events()).unwrap();
    assert!(events.iter().any(
        |e| matches!(&e.kind, EventKind::JobFailed { reason } if reason.contains("without reporting a verdict"))
    ));
}

#[tokio::test]
async fn cancelling_a_collection_stops_the_agent_and_records_it() {
    let h = Harness::with_config(&config_running("sleeping-agent.sh")).await;
    let payload = h.payload_for("x").await;
    let paths = h.job_paths();
    let mut log = EventLog::open_append(paths.events()).unwrap();
    let cancel = CancellationToken::new();
    let later = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        later.cancel();
    });

    let started = std::time::Instant::now();
    let job = the_binary().launch(&payload).unwrap();
    let outcome = collect(job, &mut log, &paths.log(), cancel).await.unwrap();

    assert!(!outcome.passed());
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "the agent was not stopped"
    );
    // Recorded by job-exec itself, not by the collector filling in a
    // missing verdict: SIGTERM cancelled the agent and the round reported it.
    let events = EventLog::read(paths.events()).unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(&e.kind, EventKind::JobFailed { reason } if reason == "cancelled")),
        "{events:?}"
    );
}
