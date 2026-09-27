use assembly_line::collect::{collect, record_launch_failure};
use assembly_line::event::{EventKind, EventLog};
use assembly_line::runner::local::LocalRunner;
use assembly_line::runner::{JobSecrets, Runner};
use assembly_line::state::JobState;
use std::io::BufRead;
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

    let running = the_binary()
        .launch(&payload, &JobSecrets::default(), &CancellationToken::new())
        .await
        .unwrap();
    let verdict = collect(running, &mut log, &paths.log(), 1, CancellationToken::new())
        .await
        .unwrap();

    assert!(verdict.passed());
    let events = EventLog::read(paths.events()).unwrap();
    assert_eq!(JobState::replay(&events), JobState::Passed);
    assert!(
        events
            .iter()
            .any(|e| matches!(e.kind, EventKind::BranchPushed { .. }))
    );
    let output = std::fs::read_to_string(paths.log()).unwrap();
    assert!(output.contains("fake-agent: write a file"), "{output}");
}

/// The envelope's whole point, end to end: an agent that prints a verdict
/// cannot make a failing round pass.
#[tokio::test]
async fn an_agent_printing_a_forged_verdict_does_not_change_the_verdict() {
    let h = Harness::with_config(&config_running("forging-agent.sh")).await;
    let payload = h.payload_for("x").await;
    let paths = h.job_paths();
    let mut log = EventLog::open_append(paths.events()).unwrap();

    let running = the_binary()
        .launch(&payload, &JobSecrets::default(), &CancellationToken::new())
        .await
        .unwrap();
    let verdict = collect(running, &mut log, &paths.log(), 1, CancellationToken::new())
        .await
        .unwrap();

    assert!(!verdict.passed());
    let events = EventLog::read(paths.events()).unwrap();
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.kind, EventKind::RoundPassed))
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

    let running = LocalRunner::using("/usr/bin/false")
        .launch(&payload, &JobSecrets::default(), &CancellationToken::new())
        .await
        .unwrap();
    let verdict = collect(running, &mut log, &paths.log(), 1, CancellationToken::new())
        .await
        .unwrap();

    assert!(!verdict.passed());
    let events = EventLog::read(paths.events()).unwrap();
    assert!(events.iter().any(
        |e| matches!(&e.kind, EventKind::RoundFailed { reason } if reason.contains("without reporting a verdict"))
    ));
}

/// A revise round that dies before its first frame never says which round it
/// was. Unrecorded, its failure would read as the previous round's, and the
/// next revise would take the same round number again.
#[tokio::test]
async fn a_round_that_dies_before_announcing_itself_is_still_recorded_as_that_round() {
    let h = Harness::new().await;
    let payload = h.payload_for("x").await;
    let paths = h.job_paths();
    let mut log = EventLog::open_append(paths.events()).unwrap();

    let running = LocalRunner::using("/usr/bin/false")
        .launch(&payload, &JobSecrets::default(), &CancellationToken::new())
        .await
        .unwrap();
    collect(running, &mut log, &paths.log(), 2, CancellationToken::new())
        .await
        .unwrap();

    let kinds: Vec<EventKind> = EventLog::read(paths.events())
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert!(
        matches!(
            kinds.as_slice(),
            [
                EventKind::RoundStarted { round: 2 },
                EventKind::RoundFailed { .. }
            ]
        ),
        "{kinds:?}"
    );
}

/// A round the runner could not even start still leaves its failure, and why.
#[tokio::test]
async fn a_runner_that_could_not_start_the_round_leaves_a_failed_round() {
    let h = Harness::new().await;
    let paths = h.job_paths();
    let mut log = EventLog::open_append(paths.events()).unwrap();

    let launch_error = LocalRunner::using("/nonexistent/assembly")
        .launch(
            &h.payload_for("x").await,
            &JobSecrets::default(),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    let verdict = record_launch_failure(&mut log, 1, &launch_error).unwrap();

    assert!(!verdict.passed());
    let events = EventLog::read(paths.events()).unwrap();
    assert!(
        events.iter().any(|e| matches!(
            &e.kind,
            EventKind::RoundFailed { reason }
                if reason.contains("could not start") && reason.contains("/nonexistent/assembly")
        )),
        "{events:?}"
    );
}

/// A collector whose output log breaks mid-stream gives up — but only once
/// `job-exec` has stopped the agent, which runs in its own process group
/// and would outlive a `job-exec` that was simply killed.
#[tokio::test]
async fn a_collector_that_fails_mid_stream_stops_the_agent_before_giving_up() {
    let h = Harness::with_config(&config_running("chatty-agent.sh")).await;
    let payload = h.payload_for("x").await;
    let paths = h.job_paths();
    let mut log = EventLog::open_append(paths.events()).unwrap();
    // A pipe whose reader hangs up once it has the agent's pid, so the
    // collector's next write to it fails.
    let output_log = h.scratch_root().with_file_name("job.log.fifo");
    let made = std::process::Command::new("mkfifo")
        .arg(&output_log)
        .status()
        .unwrap();
    assert!(made.success());
    let reader = std::thread::spawn({
        let output_log = output_log.clone();
        move || {
            std::io::BufReader::new(std::fs::File::open(output_log).unwrap())
                .lines()
                .map_while(Result::ok)
                .find_map(|line| line.split_once("chatty-agent pid=")?.1.parse::<i32>().ok())
                .expect("the agent never said its pid")
        }
    });

    let running = the_binary()
        .launch(&payload, &JobSecrets::default(), &CancellationToken::new())
        .await
        .unwrap();
    let collected = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        collect(running, &mut log, &output_log, 1, CancellationToken::new()),
    )
    .await
    .expect("the collector never gave up");
    let agent = nix::unistd::Pid::from_raw(reader.join().unwrap());

    assert!(collected.is_err());
    assert_eq!(
        nix::sys::signal::kill(agent, None),
        Err(nix::errno::Errno::ESRCH),
        "the agent outlived the collector that gave up on it"
    );
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
    let running = the_binary()
        .launch(&payload, &JobSecrets::default(), &CancellationToken::new())
        .await
        .unwrap();
    let verdict = collect(running, &mut log, &paths.log(), 1, cancel)
        .await
        .unwrap();

    assert!(!verdict.passed());
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
            .any(|e| matches!(&e.kind, EventKind::RoundFailed { reason } if reason == "cancelled")),
        "{events:?}"
    );
}
