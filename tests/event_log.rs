use assembly_line::event::{Event, EventKind, EventLog, RunStatus, read_events};

#[test]
fn appends_and_reads_back_in_order() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("events.jsonl");

    {
        let mut log = EventLog::open_append(&path).unwrap();
        log.append(EventKind::RunStarted { run_id: 1, jobs: 4 })
            .unwrap();
        log.append(EventKind::NodeStarted {
            node: "build".into(),
            round: 1,
        })
        .unwrap();
        log.append(EventKind::NodeFinished {
            node: "build".into(),
            exit_code: 0,
        })
        .unwrap();
        log.append(EventKind::RunFinished {
            status: RunStatus::Ok,
        })
        .unwrap();
    }

    let events = EventLog::read(&path).unwrap();
    assert_eq!(events.len(), 4);
    assert_eq!(events[0].kind, EventKind::RunStarted { run_id: 1, jobs: 4 });
    assert_eq!(
        events[3].kind,
        EventKind::RunFinished {
            status: RunStatus::Ok
        }
    );
    assert!(events[0].at <= events[3].at);
}

#[test]
fn reopening_appends_rather_than_truncates() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("events.jsonl");

    EventLog::open_append(&path)
        .unwrap()
        .append(EventKind::RunStarted { run_id: 1, jobs: 1 })
        .unwrap();
    EventLog::open_append(&path)
        .unwrap()
        .append(EventKind::NodeStarted {
            node: "a".into(),
            round: 1,
        })
        .unwrap();

    assert_eq!(EventLog::read(&path).unwrap().len(), 2);
}

#[test]
fn each_line_is_one_tagged_json_object() {
    // Writing into a buffer rather than a file — the sink is generic.
    let mut log = EventLog::new(Vec::new());
    log.append(EventKind::NodeFailed {
        node: "x".into(),
        reason: "exit 1".into(),
    })
    .unwrap();

    let raw = String::from_utf8(log.sink().clone()).unwrap();
    assert_eq!(raw.lines().count(), 1);

    let v: serde_json::Value = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
    assert_eq!(v["t"], "node_failed");
    assert_eq!(v["node"], "x");
    assert_eq!(v["reason"], "exit 1");
    assert!(v["at"].is_string());
}

#[test]
fn read_of_a_missing_file_is_empty() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(
        EventLog::read(tmp.path().join("nope.jsonl"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_torn_final_line_is_ignored() {
    let raw = concat!(
        r#"{"at":"2026-08-15T00:00:00Z","t":"run_started","run_id":1,"jobs":1}"#,
        "\n",
        r#"{"t":"node_st"#,
    );
    let events: Vec<Event> = read_events(raw.as_bytes()).unwrap();
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0].kind, EventKind::RunStarted { .. }));
}

#[test]
fn blank_lines_are_ignored() {
    let raw = concat!(
        r#"{"at":"2026-08-15T00:00:00Z","t":"run_started","run_id":1,"jobs":1}"#,
        "\n\n\n",
        r#"{"at":"2026-08-15T00:00:01Z","t":"run_finished","status":"ok"}"#,
        "\n",
    );
    assert_eq!(read_events(raw.as_bytes()).unwrap().len(), 2);
}

#[test]
fn node_is_reported_for_node_events_only() {
    assert_eq!(
        EventKind::NodeFailed {
            node: "a".into(),
            reason: "exit 1".into()
        }
        .node(),
        Some("a")
    );
    assert_eq!(
        EventKind::RunFinished {
            status: RunStatus::Ok
        }
        .node(),
        None
    );
}

#[test]
fn a_branch_published_event_round_trips_through_the_log() {
    let mut log = EventLog::new(Vec::new());
    log.append(EventKind::NodeBranchPublished {
        node: "impl-api".into(),
        branch: "al/run-1-impl-api".into(),
        pushed_to: Some("origin".into()),
    })
    .unwrap();

    let raw = String::from_utf8(log.sink().clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
    assert_eq!(v["t"], "node_branch_published");
    assert_eq!(v["branch"], "al/run-1-impl-api");
}
