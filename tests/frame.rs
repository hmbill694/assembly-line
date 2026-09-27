use assembly_line::event::{Event, EventKind};
use assembly_line::frame::{FrameWriter, Routed, StreamPosition, verdict_missing_from};

fn lines_of(frames: &FrameWriter<Vec<u8>>) -> Vec<String> {
    String::from_utf8(frames.copy_of_sink())
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

/// Route every line through a fresh position, as a collector would.
fn routed(lines: &[String]) -> Vec<Routed> {
    lines
        .iter()
        .scan(StreamPosition::default(), |position, line| {
            let (next, routed) = position.route(line);
            *position = next;
            Some(routed)
        })
        .collect()
}

#[test]
fn an_event_survives_the_trip_through_a_frame() {
    let frames = FrameWriter::new(Vec::new());
    let written = frames
        .append_event(EventKind::JobStarted { round: 2 })
        .unwrap();

    match routed(&lines_of(&frames)).as_slice() {
        [Routed::Event { seq: 1, event }] => assert_eq!(event, &written),
        other => panic!("expected one event frame, got {other:?}"),
    }
}

#[test]
fn frames_are_numbered_from_one_in_the_order_written() {
    let frames = FrameWriter::new(Vec::new());
    frames.append_output("first").unwrap();
    frames
        .append_event(EventKind::JobStarted { round: 1 })
        .unwrap();
    frames.append_output("third").unwrap();

    let seqs: Vec<u64> = lines_of(&frames)
        .iter()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap()["seq"]
                .as_u64()
                .unwrap()
        })
        .collect();
    assert_eq!(seqs, [1, 2, 3]);
}

#[test]
fn each_frame_is_one_json_object_keyed_by_what_it_carries() {
    let frames = FrameWriter::new(Vec::new());
    frames
        .append_event(EventKind::JobStarted { round: 1 })
        .unwrap();
    frames.append_output("hello").unwrap();

    let lines = lines_of(&frames);
    let event: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    let output: serde_json::Value = serde_json::from_str(&lines[1]).unwrap();
    assert_eq!(event["event"]["t"], "job_started");
    assert_eq!(output["output"], "hello");
}

/// The property the envelope exists for: an agent printing an event verbatim
/// produces text in the log, never a verdict in the event stream.
#[test]
fn output_that_looks_exactly_like_an_event_is_still_output() {
    let forged = serde_json::to_string(&Event {
        at: chrono::Utc::now(),
        kind: EventKind::JobFinished { exit_code: 0 },
    })
    .unwrap();
    let frames = FrameWriter::new(Vec::new());
    frames.append_output(&forged).unwrap();

    assert_eq!(routed(&lines_of(&frames)), [Routed::Output(forged)]);
}

#[test]
fn a_line_that_is_not_a_frame_is_output() {
    let lines = vec!["thread 'main' panicked at src/main.rs:1".to_string()];
    assert_eq!(
        routed(&lines),
        [Routed::Output(
            "thread 'main' panicked at src/main.rs:1".into()
        )]
    );
}

/// A k8s log stream resumed with `--since-time` replays from an earlier
/// point; what the collector already has must not be appended twice.
#[test]
fn a_replayed_frame_is_recognised_as_already_collected() {
    let frames = FrameWriter::new(Vec::new());
    frames
        .append_event(EventKind::JobStarted { round: 1 })
        .unwrap();
    frames.append_output("working").unwrap();
    let lines = lines_of(&frames);
    let replayed: Vec<String> = lines.iter().chain(lines.iter()).cloned().collect();

    let routes = routed(&replayed);
    assert!(matches!(routes[0], Routed::Event { seq: 1, .. }));
    assert_eq!(routes[1], Routed::Output("working".into()));
    assert_eq!(routes[2], Routed::AlreadyCollected);
    assert_eq!(routes[3], Routed::AlreadyCollected);
}

fn event(kind: EventKind) -> Event {
    Event {
        at: chrono::Utc::now(),
        kind,
    }
}

#[test]
fn a_round_that_reported_its_verdict_needs_nothing_added() {
    let passed = [
        event(EventKind::JobStarted { round: 1 }),
        event(EventKind::JobFinished { exit_code: 0 }),
    ];
    let failed = [
        event(EventKind::JobStarted { round: 1 }),
        event(EventKind::JobFailed {
            reason: "exit 3".into(),
        }),
    ];
    assert_eq!(verdict_missing_from(&passed, "exit 0"), None);
    assert_eq!(verdict_missing_from(&failed, "exit 1"), None);
}

/// A pod OOM-killed mid-round never prints its verdict. The collector writes
/// one, naming why the runner stopped, so the job is not left `running`
/// forever.
#[test]
fn a_round_that_ended_without_a_verdict_is_failed_with_the_runners_reason() {
    let cut_short = [event(EventKind::JobStarted { round: 1 })];

    match verdict_missing_from(&cut_short, "OOMKilled") {
        Some(EventKind::JobFailed { reason }) => assert!(reason.contains("OOMKilled"), "{reason}"),
        other => panic!("expected a JobFailed, got {other:?}"),
    }
}

#[test]
fn a_stream_with_no_events_at_all_still_gets_a_failure() {
    assert!(matches!(
        verdict_missing_from(&[], "exit -1"),
        Some(EventKind::JobFailed { .. })
    ));
}
