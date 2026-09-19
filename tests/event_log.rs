use assembly_line::event::{Event, EventKind, EventLog, read_events};

#[test]
fn appends_and_reads_back_in_order() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("events.jsonl");

    {
        let mut log = EventLog::open_append(&path).unwrap();
        log.append(EventKind::JobStarted { round: 1 }).unwrap();
        log.append(EventKind::JobCommitted {
            sha: "abc".into(),
            files: 1,
            insertions: 2,
            deletions: 0,
        })
        .unwrap();
        log.append(EventKind::JobFinished { exit_code: 0 }).unwrap();
    }

    let events = EventLog::read(&path).unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].kind, EventKind::JobStarted { round: 1 });
    assert_eq!(events[2].kind, EventKind::JobFinished { exit_code: 0 });
}

#[test]
fn reopening_appends_rather_than_truncates() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("events.jsonl");

    EventLog::open_append(&path)
        .unwrap()
        .append(EventKind::JobStarted { round: 1 })
        .unwrap();
    EventLog::open_append(&path)
        .unwrap()
        .append(EventKind::JobStarted { round: 2 })
        .unwrap();

    assert_eq!(EventLog::read(&path).unwrap().len(), 2);
}

#[test]
fn each_line_is_one_tagged_json_object() {
    // Writing into a buffer rather than a file — the sink is generic.
    let mut log = EventLog::new(Vec::new());
    log.append(EventKind::JobFailed {
        reason: "exit 1".into(),
    })
    .unwrap();

    let raw = String::from_utf8(log.sink().clone()).unwrap();
    assert_eq!(raw.lines().count(), 1);

    let v: serde_json::Value = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
    assert_eq!(v["t"], "job_failed");
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
        r#"{"at":"2026-08-15T00:00:00Z","t":"job_started","round":1}"#,
        "\n",
        r#"{"t":"job_fin"#,
    );
    let events: Vec<Event> = read_events(raw.as_bytes()).unwrap();
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0].kind, EventKind::JobStarted { .. }));
}

#[test]
fn blank_lines_are_ignored() {
    let raw = concat!(
        r#"{"at":"2026-08-15T00:00:00Z","t":"job_started","round":1}"#,
        "\n\n\n",
        r#"{"at":"2026-08-15T00:00:01Z","t":"job_finished","exit_code":0}"#,
        "\n",
    );
    assert_eq!(read_events(raw.as_bytes()).unwrap().len(), 2);
}

#[test]
fn a_branch_published_event_round_trips_through_the_log() {
    let mut log = EventLog::new(Vec::new());
    log.append(EventKind::JobBranchPublished {
        branch: "al/job-1".into(),
        pushed_to: Some("origin".into()),
    })
    .unwrap();

    let raw = String::from_utf8(log.sink().clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
    assert_eq!(v["t"], "job_branch_published");
    assert_eq!(v["branch"], "al/job-1");
    assert_eq!(v["pushed_to"], "origin");

    let read_back = read_events(raw.as_bytes()).unwrap();
    assert_eq!(
        read_back[0].kind,
        EventKind::JobBranchPublished {
            branch: "al/job-1".into(),
            pushed_to: Some("origin".into()),
        }
    );
}

/// A repository with no remote keeps its branch locally. That is a complete
/// outcome, so it is recorded rather than omitted.
#[test]
fn a_branch_that_stayed_local_records_no_remote() {
    let mut log = EventLog::new(Vec::new());
    log.append(EventKind::JobBranchPublished {
        branch: "al/job-1".into(),
        pushed_to: None,
    })
    .unwrap();

    let raw = String::from_utf8(log.sink().clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
    assert!(v["pushed_to"].is_null());
}
