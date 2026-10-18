use assembly_line::collect::{collect, record_launch_failure};
use assembly_line::event::{Event, EventKind, EventLog};
use assembly_line::frame::{Frame, FrameBody};
use assembly_line::report::JobReport;
use assembly_line::runner::local::LocalRunner;
use assembly_line::runner::{JobSecrets, RoundHandle, Runner, RunningRound, Termination};
use assembly_line::state::JobState;
use std::collections::VecDeque;
use std::io::BufRead;
use support::{Harness, config_running};
use tokio_util::sync::CancellationToken;

mod support;

fn the_binary() -> LocalRunner {
    LocalRunner::using(env!("CARGO_BIN_EXE_assembly"))
}

#[tokio::test]
async fn a_round_run_by_assembly_run_is_collected_into_the_same_log_as_before() {
    let h = Harness::new().await;
    let spec = h.launch_spec_for("write a file").await;
    let paths = h.job_paths();
    let mut log = EventLog::open_append(paths.events()).unwrap();

    let running = the_binary()
        .launch(&spec, &JobSecrets::default(), &CancellationToken::new())
        .await
        .unwrap();
    let verdict = collect(
        running,
        &mut log,
        &paths.log(),
        &paths.position(1),
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert!(verdict.passed());
    let events = EventLog::read(paths.events()).unwrap();
    assert_eq!(
        JobReport::from_events(paths.id.into(), &events).state,
        JobState::Passed
    );
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
    let spec = h.launch_spec_for("x").await;
    let paths = h.job_paths();
    let mut log = EventLog::open_append(paths.events()).unwrap();

    let running = the_binary()
        .launch(&spec, &JobSecrets::default(), &CancellationToken::new())
        .await
        .unwrap();
    let verdict = collect(
        running,
        &mut log,
        &paths.log(),
        &paths.position(1),
        CancellationToken::new(),
    )
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

/// A round that exits without printing a single frame — what a pod
/// OOM-killed before its first event looks like from the collector.
#[tokio::test]
async fn a_round_that_dies_without_a_verdict_is_recorded_as_failed() {
    let h = Harness::new().await;
    let spec = h.launch_spec_for("x").await;
    let paths = h.job_paths();
    let mut log = EventLog::open_append(paths.events()).unwrap();

    let running = LocalRunner::using("/usr/bin/false")
        .launch(&spec, &JobSecrets::default(), &CancellationToken::new())
        .await
        .unwrap();
    let verdict = collect(
        running,
        &mut log,
        &paths.log(),
        &paths.position(1),
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert!(!verdict.passed());
    let events = EventLog::read(paths.events()).unwrap();
    assert!(events.iter().any(
        |e| matches!(&e.kind, EventKind::RoundFailed { reason } if reason.contains("without reporting a verdict"))
    ));
}

/// A round the runner could not even start still leaves its failure, and why.
#[tokio::test]
async fn a_runner_that_could_not_start_the_round_leaves_a_failed_round() {
    let h = Harness::new().await;
    let paths = h.job_paths();
    let mut log = EventLog::open_append(paths.events()).unwrap();

    let launch_error = LocalRunner::using("/nonexistent/assembly")
        .launch(
            &h.launch_spec_for("x").await,
            &JobSecrets::default(),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    let verdict = record_launch_failure(&mut log, &launch_error).unwrap();

    assert!(!verdict.passed());
    let events = EventLog::read(paths.events()).unwrap();
    assert!(
        matches!(
            events.as_slice(),
            [e] if matches!(
                &e.kind,
                EventKind::RoundFailed { reason }
                    if reason.contains("could not start") && reason.contains("/nonexistent/assembly")
            )
        ),
        "{events:?}"
    );
}

/// A collector whose output log breaks mid-stream gives up — but only once
/// `run` has stopped the agent, which runs in its own process group and
/// would outlive a `run` that was simply killed.
#[tokio::test]
async fn a_collector_that_fails_mid_stream_stops_the_agent_before_giving_up() {
    let h = Harness::with_config(&config_running("chatty-agent.sh")).await;
    let spec = h.launch_spec_for("x").await;
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
        .launch(&spec, &JobSecrets::default(), &CancellationToken::new())
        .await
        .unwrap();
    let collected = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        collect(
            running,
            &mut log,
            &output_log,
            &paths.position(1),
            CancellationToken::new(),
        ),
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
    let spec = h.launch_spec_for("x").await;
    let paths = h.job_paths();
    let mut log = EventLog::open_append(paths.events()).unwrap();
    let cancel = CancellationToken::new();
    let later = cancel.clone();
    let output = paths.log();
    tokio::spawn(async move {
        // Once the agent runs: a cancel while `run` is still cloning ends it
        // before it can report a round at all.
        while !std::fs::read_to_string(&output).is_ok_and(|log| log.contains("sleeping-agent:")) {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        later.cancel();
    });

    let started = std::time::Instant::now();
    let running = the_binary()
        .launch(&spec, &JobSecrets::default(), &CancellationToken::new())
        .await
        .unwrap();
    let verdict = collect(running, &mut log, &paths.log(), &paths.position(1), cancel)
        .await
        .unwrap();

    assert!(!verdict.passed());
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "the agent was not stopped"
    );
    // Recorded by `run` itself, not by the collector filling in a missing
    // verdict: SIGTERM cancelled the agent and the round reported it.
    let events = EventLog::read(paths.events()).unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(&e.kind, EventKind::RoundFailed { reason } if reason == "cancelled")),
        "{events:?}"
    );
}

/// A round's stream as a fixed list of lines, for feeding the collector.
struct Replayed {
    lines: VecDeque<String>,
}

impl RunningRound for Replayed {
    fn next_line(&mut self) -> impl Future<Output = Option<String>> + Send {
        std::future::ready(self.lines.pop_front())
    }
    fn cancel(&mut self) -> impl Future<Output = ()> + Send {
        std::future::ready(())
    }
    fn termination(self) -> impl Future<Output = Termination> + Send {
        std::future::ready(Termination::Exited(0))
    }
    fn handle(&self) -> RoundHandle {
        RoundHandle::Docker {
            container: "replayed".into(),
        }
    }
}

fn frame_line(seq: u64, body: FrameBody) -> String {
    serde_json::to_string(&Frame { seq, body }).unwrap()
}

/// A stream replayed from its start after a reconnect adds nothing twice —
/// not the frames, and not the lines between them that are not frames.
#[tokio::test]
async fn a_resumed_collection_skips_what_it_already_has() {
    let dir = tempfile::tempdir().unwrap();
    let (events, output, position) = (
        dir.path().join("events.jsonl"),
        dir.path().join("job.log"),
        dir.path().join("position"),
    );
    let passed = Event {
        at: chrono::Utc::now(),
        kind: EventKind::RoundPassed,
    };
    let stream = [
        frame_line(1, FrameBody::Output("one".into())),
        "run's own stderr".to_string(),
        frame_line(2, FrameBody::Output("two".into())),
        frame_line(3, FrameBody::Output("three".into())),
        frame_line(4, FrameBody::Event(passed)),
    ];
    let mut log = EventLog::open_append(&events).unwrap();
    let first_three = Replayed {
        lines: stream[..3].iter().cloned().collect(),
    };
    collect(
        first_three,
        &mut log,
        &output,
        &position,
        CancellationToken::new(),
    )
    .await
    .unwrap();

    let everything_again = Replayed {
        lines: stream.iter().cloned().collect(),
    };
    let verdict = collect(
        everything_again,
        &mut log,
        &output,
        &position,
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert!(verdict.passed());
    assert_eq!(
        std::fs::read_to_string(&output).unwrap(),
        "one\nrun's own stderr\ntwo\nthree\n"
    );
    assert_eq!(
        EventLog::read(&events).unwrap().len(),
        2,
        "the first collection's missing verdict, then the second's real one"
    );
}

/// `run` leads a session of its own. A reattached round's pid that no longer
/// does has been handed to some other process, which must not hold the
/// collection open once the round's frames are drained.
#[tokio::test]
async fn a_reattached_local_rounds_pid_taken_by_another_process_does_not_keep_it_open() {
    let dir = tempfile::tempdir().unwrap();
    let frames = dir.path().join("round-1.frames");
    let passed = Event {
        at: chrono::Utc::now(),
        kind: EventKind::RoundPassed,
    };
    std::fs::write(
        &frames,
        format!(
            "{}\n{}\n",
            frame_line(1, FrameBody::Output("one".into())),
            frame_line(2, FrameBody::Event(passed))
        ),
    )
    .unwrap();
    let mut stranger = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let handle = RoundHandle::Local {
        pid: i32::try_from(stranger.id()).unwrap(),
        frames,
    };
    let mut log = EventLog::open_append(dir.path().join("events.jsonl")).unwrap();

    let running = the_binary().reattach(&handle).await.unwrap();
    let collected = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        collect(
            running,
            &mut log,
            &dir.path().join("job.log"),
            &dir.path().join("round-1.position"),
            CancellationToken::new(),
        ),
    )
    .await;
    let _ = stranger.kill();
    let _ = stranger.wait();

    assert!(
        collected
            .expect("the collection waited on a stranger")
            .unwrap()
            .passed()
    );
}

/// A local round's handle is enough to collect it from a process that did
/// not launch it: its frames file, read from the start, and its pid.
#[tokio::test]
async fn a_local_round_is_reattached_by_its_frames_file() {
    let h = Harness::new().await;
    let spec = h.launch_spec_for("write a file").await;
    let paths = h.job_paths();
    let mut log = EventLog::open_append(paths.events()).unwrap();
    let first = the_binary()
        .launch(&spec, &JobSecrets::default(), &CancellationToken::new())
        .await
        .unwrap();
    let handle = first.handle();
    drop(first);

    let again = the_binary().reattach(&handle).await.unwrap();
    let verdict = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        collect(
            again,
            &mut log,
            &paths.log(),
            &paths.position(1),
            CancellationToken::new(),
        ),
    )
    .await
    .expect("the reattached round was never seen to end")
    .unwrap();

    assert!(verdict.passed());
    let output = std::fs::read_to_string(paths.log()).unwrap();
    assert_eq!(
        output.matches("fake-agent: write a file").count(),
        1,
        "{output}"
    );
}
