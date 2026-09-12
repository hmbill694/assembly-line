use assembly_line::event::{Event, EventKind, RunStatus};
use assembly_line::report::RunReport;
use assembly_line::state::NodeState;
use chrono::{DateTime, TimeDelta, Utc};

fn ids() -> Vec<String> {
    vec!["build".into(), "impl-auth".into(), "wire-routes".into()]
}

/// Build a timeline where each event is stamped at a fixed offset in seconds.
fn timeline(entries: Vec<(i64, EventKind)>) -> Vec<Event> {
    let origin: DateTime<Utc> = Utc::now();
    entries
        .into_iter()
        .map(|(offset, kind)| Event {
            at: origin + TimeDelta::seconds(offset),
            kind,
        })
        .collect()
}

fn started(node: &str, round: u32) -> EventKind {
    EventKind::NodeStarted {
        node: node.into(),
        round,
    }
}

#[test]
fn summarizes_states_durations_and_detail() {
    let events = timeline(vec![
        (0, EventKind::RunStarted { run_id: 7, jobs: 4 }),
        (0, started("build", 1)),
        (
            2,
            EventKind::NodeFinished {
                node: "build".into(),
                exit_code: 0,
            },
        ),
        (2, started("impl-auth", 1)),
        (
            9,
            EventKind::NodeFailed {
                node: "impl-auth".into(),
                reason: "exit 1".into(),
            },
        ),
        (
            9,
            EventKind::RunFinished {
                status: RunStatus::Partial,
            },
        ),
    ]);

    let report = RunReport::from_events(7, &ids(), &events);
    assert_eq!(report.id, 7);
    assert_eq!(report.status, Some(RunStatus::Partial));

    assert_eq!(report.nodes[0].state, NodeState::Done);
    assert_eq!(report.nodes[0].duration.unwrap().as_secs(), 2);
    assert!(report.nodes[0].detail.is_none());

    assert_eq!(report.nodes[1].state, NodeState::Failed);
    assert_eq!(report.nodes[1].duration.unwrap().as_secs(), 7);
    assert_eq!(report.nodes[1].detail.as_deref(), Some("exit 1"));

    // A node with no events of its own is left pending.
    assert_eq!(report.nodes[2].state, NodeState::Pending);
}

#[test]
fn a_retried_node_reports_its_last_attempt() {
    let events = timeline(vec![
        (0, started("build", 1)),
        (10, started("build", 2)),
        (
            13,
            EventKind::NodeFinished {
                node: "build".into(),
                exit_code: 0,
            },
        ),
    ]);

    let report = RunReport::from_events(1, &ids(), &events);
    assert_eq!(report.nodes[0].duration.unwrap().as_secs(), 3);
}

#[test]
fn a_retry_clears_the_previous_failure_reason() {
    let events = timeline(vec![
        (0, started("build", 1)),
        (
            1,
            EventKind::NodeFailed {
                node: "build".into(),
                reason: "exit 1".into(),
            },
        ),
        (2, started("build", 2)),
        (
            3,
            EventKind::NodeFinished {
                node: "build".into(),
                exit_code: 0,
            },
        ),
    ]);

    let report = RunReport::from_events(1, &ids(), &events);
    assert_eq!(report.nodes[0].state, NodeState::Done);
    assert!(report.nodes[0].detail.is_none(), "stale reason survived");
}

#[test]
fn nodes_with_no_events_are_pending() {
    let report = RunReport::from_events(1, &ids(), &[]);
    assert!(report.nodes.iter().all(|n| n.state == NodeState::Pending));
    assert_eq!(report.nodes.len(), 3);
    assert!(report.status.is_none());
}

#[test]
fn an_unfinished_run_has_no_status() {
    let events = timeline(vec![(0, started("build", 1))]);
    let report = RunReport::from_events(1, &ids(), &events);

    assert!(report.status.is_none());
    assert_eq!(report.nodes[0].state, NodeState::Running);
}

#[test]
fn nodes_appear_in_declared_order_not_alphabetical() {
    let declared = vec!["zeta".to_string(), "alpha".to_string()];
    let report = RunReport::from_events(1, &declared, &[]);
    assert_eq!(
        report.nodes.iter().map(|n| &n.id).collect::<Vec<_>>(),
        vec!["zeta", "alpha"]
    );
}

#[test]
fn counts_by_state() {
    let events = timeline(vec![
        (
            0,
            EventKind::NodeFinished {
                node: "build".into(),
                exit_code: 0,
            },
        ),
        (
            0,
            EventKind::NodeFailed {
                node: "impl-auth".into(),
                reason: "exit 1".into(),
            },
        ),
    ]);

    let report = RunReport::from_events(1, &ids(), &events);
    assert_eq!(report.count_in_state(NodeState::Done), 1);
    assert_eq!(report.count_in_state(NodeState::Failed), 1);
    assert_eq!(report.count_in_state(NodeState::Pending), 1);
}

#[test]
fn the_tree_shows_every_node_and_the_summary_line() {
    let events = timeline(vec![
        (0, started("build", 1)),
        (
            1,
            EventKind::NodeFinished {
                node: "build".into(),
                exit_code: 0,
            },
        ),
        (
            1,
            EventKind::RunFinished {
                status: RunStatus::Partial,
            },
        ),
    ]);

    let tree = RunReport::from_events(3, &ids(), &events).to_terminal_tree();

    ids()
        .iter()
        .for_each(|id| assert!(tree.contains(id.as_str()), "{id} missing from:\n{tree}"));
    assert!(tree.contains("run 3: partial"), "{tree}");
    assert!(tree.contains("1 done"), "{tree}");
}

#[test]
fn an_in_progress_run_says_so() {
    let report = RunReport::from_events(4, &ids(), &[]);
    assert!(report.to_summary_line().contains("in progress"));
}

#[test]
fn a_node_reports_the_diff_it_committed() {
    let events = timeline(vec![
        (0, started("build", 1)),
        (
            1,
            EventKind::NodeCommitted {
                node: "build".into(),
                sha: "abc".into(),
                files: 3,
                insertions: 120,
                deletions: 4,
            },
        ),
        (
            2,
            EventKind::NodeFinished {
                node: "build".into(),
                exit_code: 0,
            },
        ),
    ]);

    let report = RunReport::from_events(1, &ids(), &events);
    let diff = report.nodes[0].diff.expect("a diff summary");
    assert_eq!((diff.files, diff.insertions, diff.deletions), (3, 120, 4));
    assert!(report.to_terminal_tree().contains("+120"));
}

#[test]
fn a_node_that_committed_nothing_reports_no_diff() {
    let events = timeline(vec![
        (0, started("build", 1)),
        (
            1,
            EventKind::NodeFinished {
                node: "build".into(),
                exit_code: 0,
            },
        ),
    ]);

    let report = RunReport::from_events(1, &ids(), &events);
    assert!(report.nodes[0].diff.is_none());
}
