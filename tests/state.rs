use assembly_line::event::{Event, EventKind, RunStatus};
use assembly_line::state::{Counts, NodeState, RunState};

fn ids() -> Vec<String> {
    vec!["build".into(), "left".into(), "right".into()]
}

fn finished(node: &str) -> EventKind {
    EventKind::NodeFinished {
        node: node.into(),
        exit_code: 0,
    }
}

fn started(node: &str) -> EventKind {
    EventKind::NodeStarted {
        node: node.into(),
        round: 1,
    }
}

#[test]
fn every_task_starts_pending() {
    let st = RunState::new(&ids());
    assert_eq!(st.state("build"), NodeState::Pending);
    assert_eq!(st.state("left"), NodeState::Pending);
    assert_eq!(st.state("right"), NodeState::Pending);
}

#[test]
fn starting_and_finishing_a_node_moves_it_through_running_to_done() {
    let mut st = RunState::new(&ids());

    st.apply(&started("build"));
    assert_eq!(st.state("build"), NodeState::Running);

    st.apply(&finished("build"));
    assert_eq!(st.state("build"), NodeState::Done);
}

#[test]
fn a_failed_node_is_recorded_as_failed() {
    let mut st = RunState::new(&ids());
    st.apply(&EventKind::NodeFailed {
        node: "build".into(),
        reason: "exit 1".into(),
    });
    assert_eq!(st.state("build"), NodeState::Failed);
}

#[test]
fn replay_reconstructs_state_and_reset_running_requeues() {
    let now = chrono::Utc::now();
    let events: Vec<Event> = [
        EventKind::RunStarted { run_id: 1, jobs: 4 },
        started("build"),
        finished("build"),
        started("left"),
    ]
    .into_iter()
    .map(|kind| Event { at: now, kind })
    .collect();

    let mut st = RunState::replay(&ids(), &events);
    assert_eq!(st.state("build"), NodeState::Done);
    assert_eq!(st.state("left"), NodeState::Running);

    assert_eq!(st.reset_running(), vec!["left".to_string()]);
    assert_eq!(st.state("left"), NodeState::Pending);
}

#[test]
fn counts_summarize_the_run() {
    let mut st = RunState::new(&ids());

    st.apply(&finished("build"));
    st.apply(&EventKind::NodeFailed {
        node: "left".into(),
        reason: "exit 1".into(),
    });

    assert_eq!(
        st.counts(),
        Counts {
            done: 1,
            failed: 1,
            outstanding: 1,
        }
    );
}

#[test]
fn run_finished_is_recorded() {
    let mut st = RunState::new(&ids());

    assert!(st.status.is_none());
    st.apply(&EventKind::RunFinished {
        status: RunStatus::Partial,
    });
    assert_eq!(st.status, Some(RunStatus::Partial));
}

#[test]
fn a_commit_event_does_not_change_node_state() {
    let mut st = RunState::new(&ids());

    st.apply(&started("build"));
    st.apply(&EventKind::NodeCommitted {
        node: "build".into(),
        sha: "abc".into(),
        files: 2,
        insertions: 10,
        deletions: 1,
    });
    assert_eq!(
        st.state("build"),
        NodeState::Running,
        "a commit is not completion"
    );

    st.apply(&finished("build"));
    assert_eq!(st.state("build"), NodeState::Done);
}
