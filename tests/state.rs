use assembly_line::config::parse_graph;
use assembly_line::dag::Dag;
use assembly_line::event::{Event, EventKind, RunStatus};
use assembly_line::state::{Counts, NodeState, RunState, task_map};

const DIAMOND: &str = r#"
[[task]]
id = "build"
kind = "shell"
run = "true"

[[task]]
id = "left"
kind = "shell"
needs = ["build"]
run = "true"

[[task]]
id = "right"
kind = "shell"
needs = ["build"]
run = "true"

[[task]]
id = "join"
kind = "shell"
needs = ["left", "right"]
run = "true"
"#;

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
fn only_root_nodes_are_ready_initially() {
    let g = parse_graph(DIAMOND).unwrap();
    let dag = Dag::build(&g.tasks).unwrap();
    let tm = task_map(&g.tasks);
    let st = RunState::new(dag.ids());
    assert_eq!(st.ready(&dag, &tm), vec!["build".to_string()]);
}

#[test]
fn finishing_a_node_unlocks_both_branches() {
    let g = parse_graph(DIAMOND).unwrap();
    let dag = Dag::build(&g.tasks).unwrap();
    let tm = task_map(&g.tasks);
    let mut st = RunState::new(dag.ids());

    st.apply(&started("build"));
    assert_eq!(st.state("build"), NodeState::Running);
    assert!(st.ready(&dag, &tm).is_empty());

    st.apply(&finished("build"));
    assert_eq!(st.state("build"), NodeState::Done);
    assert_eq!(
        st.ready(&dag, &tm),
        vec!["left".to_string(), "right".to_string()]
    );
}

#[test]
fn a_join_waits_for_every_dependency() {
    let g = parse_graph(DIAMOND).unwrap();
    let dag = Dag::build(&g.tasks).unwrap();
    let tm = task_map(&g.tasks);
    let mut st = RunState::new(dag.ids());

    ["build", "left"]
        .iter()
        .for_each(|n| st.apply(&finished(n)));
    assert_eq!(st.ready(&dag, &tm), vec!["right".to_string()]);

    st.apply(&finished("right"));
    assert_eq!(st.ready(&dag, &tm), vec!["join".to_string()]);
}

#[test]
fn a_skipped_dependency_never_satisfies() {
    let g = parse_graph(DIAMOND).unwrap();
    let dag = Dag::build(&g.tasks).unwrap();
    let tm = task_map(&g.tasks);
    let mut st = RunState::new(dag.ids());

    st.apply(&finished("build"));
    st.apply(&finished("left"));
    st.apply(&EventKind::NodeSkipped {
        node: "right".into(),
        because: "needs x".into(),
    });
    assert!(st.ready(&dag, &tm).is_empty());
}

#[test]
fn a_failed_dependency_with_on_failure_continue_does_satisfy() {
    let src = r#"
[[task]]
id = "lint"
kind = "shell"
run = "false"
on_failure = "continue"

[[task]]
id = "build"
kind = "shell"
needs = ["lint"]
run = "true"
"#;
    let g = parse_graph(src).unwrap();
    let dag = Dag::build(&g.tasks).unwrap();
    let tm = task_map(&g.tasks);
    let mut st = RunState::new(dag.ids());

    st.apply(&EventKind::NodeFailed {
        node: "lint".into(),
        reason: "exit 1".into(),
    });
    assert_eq!(st.state("lint"), NodeState::Failed);
    assert_eq!(st.ready(&dag, &tm), vec!["build".to_string()]);
}

#[test]
fn a_failed_dependency_without_continue_blocks() {
    let g = parse_graph(DIAMOND).unwrap();
    let dag = Dag::build(&g.tasks).unwrap();
    let tm = task_map(&g.tasks);
    let mut st = RunState::new(dag.ids());

    st.apply(&EventKind::NodeFailed {
        node: "build".into(),
        reason: "exit 1".into(),
    });
    assert!(st.ready(&dag, &tm).is_empty());
}

#[test]
fn replay_reconstructs_state_and_reset_running_requeues() {
    let g = parse_graph(DIAMOND).unwrap();
    let dag = Dag::build(&g.tasks).unwrap();
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

    let mut st = RunState::replay(dag.ids(), &events);
    assert_eq!(st.state("build"), NodeState::Done);
    assert_eq!(st.state("left"), NodeState::Running);

    assert_eq!(st.reset_running(), vec!["left".to_string()]);
    assert_eq!(st.state("left"), NodeState::Pending);

    let tm = task_map(&g.tasks);
    assert_eq!(
        st.ready(&dag, &tm),
        vec!["left".to_string(), "right".to_string()]
    );
}

#[test]
fn counts_summarize_the_run() {
    let g = parse_graph(DIAMOND).unwrap();
    let dag = Dag::build(&g.tasks).unwrap();
    let mut st = RunState::new(dag.ids());

    st.apply(&finished("build"));
    st.apply(&EventKind::NodeFailed {
        node: "left".into(),
        reason: "exit 1".into(),
    });
    st.apply(&EventKind::NodeSkipped {
        node: "join".into(),
        because: "needs left".into(),
    });

    assert_eq!(
        st.counts(),
        Counts {
            done: 1,
            failed: 1,
            skipped: 1,
            outstanding: 1,
        }
    );
}

#[test]
fn run_finished_is_recorded() {
    let g = parse_graph(DIAMOND).unwrap();
    let dag = Dag::build(&g.tasks).unwrap();
    let mut st = RunState::new(dag.ids());

    assert!(st.status.is_none());
    st.apply(&EventKind::RunFinished {
        status: RunStatus::Partial,
    });
    assert_eq!(st.status, Some(RunStatus::Partial));
}
