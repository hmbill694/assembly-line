use assembly_line::config::{Task, parse_graph};
use assembly_line::dag::{Dag, ValidationError, Warning, validate};

fn tasks(src: &str) -> Vec<Task> {
    parse_graph(src).unwrap().tasks
}

const DIAMOND: &str = r#"
[[task]]
id = "build"
prompt = "x"

[[task]]
id = "left"
needs = ["build"]
prompt = "x"

[[task]]
id = "right"
needs = ["build"]
prompt = "x"

[[task]]
id = "join"
needs = ["left", "right"]
prompt = "x"
"#;

#[test]
fn builds_a_valid_dag_and_reports_descendants() {
    let dag = Dag::build(&tasks(DIAMOND)).expect("valid");
    assert_eq!(dag.ids(), ["build", "left", "right", "join"]);
    assert_eq!(dag.needs("join"), ["left", "right"]);

    let d: Vec<String> = dag.descendants("build").into_iter().collect();
    assert_eq!(
        d,
        vec!["join".to_string(), "left".to_string(), "right".to_string()]
    );
    assert!(dag.descendants("join").is_empty());
}

#[test]
fn descendants_reach_the_far_end_of_a_long_chain() {
    let src: String = (0..12)
        .map(|i| match i {
            0 => "[[task]]\nid=\"n0\"\nprompt=\"x\"\n".to_string(),
            n => format!(
                "[[task]]\nid=\"n{n}\"\nneeds=[\"n{}\"]\nprompt=\"x\"\n",
                n - 1
            ),
        })
        .collect();
    let dag = Dag::build(&tasks(&src)).expect("valid");
    assert_eq!(dag.descendants("n0").len(), 11);
    assert!(dag.descendants("n0").contains("n11"));
}

#[test]
fn rejects_duplicate_ids() {
    let src = "[[task]]\nid=\"a\"\nprompt=\"x\"\n\
               [[task]]\nid=\"a\"\nprompt=\"x\"\n";
    let errs = Dag::build(&tasks(src)).unwrap_err();
    assert!(
        errs.contains(&ValidationError::DuplicateId("a".into())),
        "{errs:?}"
    );
}

#[test]
fn rejects_unknown_dependency() {
    let src = "[[task]]\nid=\"a\"\nneeds=[\"ghost\"]\nprompt=\"x\"\n";
    let errs = Dag::build(&tasks(src)).unwrap_err();
    assert!(
        errs.contains(&ValidationError::UnknownDep {
            task: "a".into(),
            dep: "ghost".into()
        }),
        "{errs:?}"
    );
}

#[test]
fn rejects_a_cycle() {
    let src = "[[task]]\nid=\"a\"\nneeds=[\"b\"]\nprompt=\"x\"\n\
               [[task]]\nid=\"b\"\nneeds=[\"a\"]\nprompt=\"x\"\n";
    let errs = Dag::build(&tasks(src)).unwrap_err();
    let cycle = errs
        .iter()
        .find_map(|e| match e {
            ValidationError::Cycle(p) => Some(p.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected a cycle error, got {errs:?}"));
    assert!(cycle.contains(&"a".to_string()) && cycle.contains(&"b".to_string()));
    assert_eq!(cycle.first(), cycle.last(), "cycle should be a closed loop");
}

#[test]
fn detects_a_cycle_downstream_of_valid_nodes() {
    let src = "[[task]]\nid=\"root\"\nprompt=\"x\"\n\
               [[task]]\nid=\"a\"\nneeds=[\"root\",\"b\"]\nprompt=\"x\"\n\
               [[task]]\nid=\"b\"\nneeds=[\"a\"]\nprompt=\"x\"\n";
    let errs = Dag::build(&tasks(src)).unwrap_err();
    assert!(
        errs.iter().any(|e| matches!(e, ValidationError::Cycle(_))),
        "{errs:?}"
    );
}

#[test]
fn accepts_a_wide_diamond_chain_without_blowing_up() {
    // Eight stacked diamonds: exponential for a naive recursive walk.
    let src: String = (0..8)
        .map(|i| {
            format!(
                "[[task]]\nid=\"a{i}\"\nneeds=[{prev}]\nprompt=\"x\"\n\
                 [[task]]\nid=\"b{i}\"\nneeds=[{prev}]\nprompt=\"x\"\n\
                 [[task]]\nid=\"j{i}\"\nneeds=[\"a{i}\",\"b{i}\"]\nprompt=\"x\"\n",
                prev = if i == 0 {
                    String::new()
                } else {
                    format!("\"j{}\"", i - 1)
                }
            )
        })
        .collect();
    let dag = Dag::build(&tasks(&src)).expect("valid");
    assert!(dag.descendants("j0").contains("j7"));
}

#[test]
fn rejects_self_dependency() {
    let src = "[[task]]\nid=\"a\"\nneeds=[\"a\"]\nprompt=\"x\"\n";
    let errs = Dag::build(&tasks(src)).unwrap_err();
    assert!(
        errs.contains(&ValidationError::SelfDep("a".into())),
        "{errs:?}"
    );
}

#[test]
fn rejects_ids_that_are_unsafe_as_paths() {
    let src = "[[task]]\nid=\"../etc\"\nprompt=\"x\"\n";
    let errs = Dag::build(&tasks(src)).unwrap_err();
    assert!(
        errs.contains(&ValidationError::InvalidId("../etc".into())),
        "{errs:?}"
    );
}

#[test]
fn a_task_without_a_prompt_is_rejected() {
    let graph = parse_graph(
        r#"
        [[task]]
        id = "impl"
        "#,
    )
    .unwrap();

    let validation = validate(&graph);
    assert!(
        validation
            .errors
            .iter()
            .any(|e| matches!(e, ValidationError::AgentMissingPrompt(id) if id == "impl")),
        "expected a missing-prompt error, got {:?}",
        validation.errors
    );
}

#[test]
fn reports_every_problem_at_once() {
    let src = "[[task]]\nid=\"a\"\n\
               [[task]]\nid=\"b\"\nneeds=[\"ghost\"]\nprompt=\"x\"\n";
    let errs = Dag::build(&tasks(src)).unwrap_err();
    assert_eq!(errs.len(), 2, "{errs:?}");
}

#[test]
fn rejects_unknown_provider() {
    let src = "[[task]]\nid=\"a\"\nprompt=\"hi\"\nprovider=\"nope\"\n";
    let v = validate(&parse_graph(src).unwrap());
    assert!(
        v.errors.contains(&ValidationError::UnknownProvider {
            task: "a".into(),
            provider: "nope".into()
        }),
        "{:?}",
        v.errors
    );
    assert!(v.dag.is_none());
}

#[test]
fn warns_on_agent_without_verify() {
    let src = "[providers.p]\ncmd=\"true\"\n\
               [[task]]\nid=\"a\"\nprompt=\"hi\"\nprovider=\"p\"\n";
    let v = validate(&parse_graph(src).unwrap());
    assert!(v.errors.is_empty(), "{:?}", v.errors);
    assert!(
        v.warnings
            .contains(&Warning::AgentWithoutVerify("a".into())),
        "{:?}",
        v.warnings
    );
}

#[test]
fn warns_when_cost_cap_has_no_adapter() {
    let src = "[providers.p]\ncmd=\"true\"\n\
               [[task]]\nid=\"a\"\nprompt=\"hi\"\nprovider=\"p\"\n\
               verify=\"true\"\nmax_cost_usd=5.0\n";
    let v = validate(&parse_graph(src).unwrap());
    assert!(
        v.warnings.contains(&Warning::CostCapWithoutAdapter {
            task: "a".into(),
            provider: "p".into()
        }),
        "{:?}",
        v.warnings
    );
}

#[test]
fn no_cost_warning_when_the_provider_has_an_adapter() {
    let src = "[providers.p]\ncmd=\"true\"\nadapter=\"wrap.sh\"\n\
               [[task]]\nid=\"a\"\nprompt=\"hi\"\nprovider=\"p\"\n\
               verify=\"true\"\nmax_cost_usd=5.0\n";
    let v = validate(&parse_graph(src).unwrap());
    assert!(v.warnings.is_empty(), "{:?}", v.warnings);
}

#[test]
fn rejects_invalid_max_duration() {
    let src = "[[task]]\nid=\"a\"\nprompt=\"x\"\nmax_duration=\"soon\"\n";
    let v = validate(&parse_graph(src).unwrap());
    assert!(
        v.errors
            .iter()
            .any(|e| matches!(e, ValidationError::InvalidDuration { .. })),
        "{:?}",
        v.errors
    );
}

#[test]
fn rejects_an_id_that_would_collide_with_the_integration_worktree() {
    let src = "[[task]]\nid=\"_integration\"\nprompt=\"x\"\n";
    let errs = Dag::build(&tasks(src)).unwrap_err();
    assert!(
        errs.contains(&ValidationError::ReservedId("_integration".into())),
        "{errs:?}"
    );
}
