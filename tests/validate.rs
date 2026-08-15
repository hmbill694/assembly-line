use assembly_line::config::{Task, parse_graph};
use assembly_line::dag::{Dag, ValidationError, Warning, validate};

fn tasks(src: &str) -> Vec<Task> {
    parse_graph(src).unwrap().tasks
}

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
            0 => "[[task]]\nid=\"n0\"\nkind=\"shell\"\nrun=\"true\"\n".to_string(),
            n => format!(
                "[[task]]\nid=\"n{n}\"\nkind=\"shell\"\nneeds=[\"n{}\"]\nrun=\"true\"\n",
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
    let src = "[[task]]\nid=\"a\"\nkind=\"shell\"\nrun=\"true\"\n\
               [[task]]\nid=\"a\"\nkind=\"shell\"\nrun=\"true\"\n";
    let errs = Dag::build(&tasks(src)).unwrap_err();
    assert!(
        errs.contains(&ValidationError::DuplicateId("a".into())),
        "{errs:?}"
    );
}

#[test]
fn rejects_unknown_dependency() {
    let src = "[[task]]\nid=\"a\"\nkind=\"shell\"\nneeds=[\"ghost\"]\nrun=\"true\"\n";
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
    let src = "[[task]]\nid=\"a\"\nkind=\"shell\"\nneeds=[\"b\"]\nrun=\"true\"\n\
               [[task]]\nid=\"b\"\nkind=\"shell\"\nneeds=[\"a\"]\nrun=\"true\"\n";
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
    let src = "[[task]]\nid=\"root\"\nkind=\"shell\"\nrun=\"true\"\n\
               [[task]]\nid=\"a\"\nkind=\"shell\"\nneeds=[\"root\",\"b\"]\nrun=\"true\"\n\
               [[task]]\nid=\"b\"\nkind=\"shell\"\nneeds=[\"a\"]\nrun=\"true\"\n";
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
                "[[task]]\nid=\"a{i}\"\nkind=\"shell\"\nneeds=[{prev}]\nrun=\"true\"\n\
                 [[task]]\nid=\"b{i}\"\nkind=\"shell\"\nneeds=[{prev}]\nrun=\"true\"\n\
                 [[task]]\nid=\"j{i}\"\nkind=\"shell\"\nneeds=[\"a{i}\",\"b{i}\"]\nrun=\"true\"\n",
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
    let src = "[[task]]\nid=\"a\"\nkind=\"shell\"\nneeds=[\"a\"]\nrun=\"true\"\n";
    let errs = Dag::build(&tasks(src)).unwrap_err();
    assert!(
        errs.contains(&ValidationError::SelfDep("a".into())),
        "{errs:?}"
    );
}

#[test]
fn rejects_ids_that_are_unsafe_as_paths() {
    let src = "[[task]]\nid=\"../etc\"\nkind=\"shell\"\nrun=\"true\"\n";
    let errs = Dag::build(&tasks(src)).unwrap_err();
    assert!(
        errs.contains(&ValidationError::InvalidId("../etc".into())),
        "{errs:?}"
    );
}

#[test]
fn rejects_shell_task_without_run_and_agent_without_prompt() {
    let src = "[[task]]\nid=\"a\"\nkind=\"shell\"\n\
               [[task]]\nid=\"b\"\nkind=\"agent\"\n";
    let errs = Dag::build(&tasks(src)).unwrap_err();
    assert!(
        errs.contains(&ValidationError::ShellMissingRun("a".into())),
        "{errs:?}"
    );
    assert!(
        errs.contains(&ValidationError::AgentMissingPrompt("b".into())),
        "{errs:?}"
    );
}

#[test]
fn reports_every_problem_at_once() {
    let src = "[[task]]\nid=\"a\"\nkind=\"shell\"\n\
               [[task]]\nid=\"b\"\nkind=\"shell\"\nneeds=[\"ghost\"]\nrun=\"true\"\n";
    let errs = Dag::build(&tasks(src)).unwrap_err();
    assert_eq!(errs.len(), 2, "{errs:?}");
}

#[test]
fn rejects_unknown_provider() {
    let src = "[[task]]\nid=\"a\"\nkind=\"agent\"\nprompt=\"hi\"\nprovider=\"nope\"\n";
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
fn warns_on_unsupervised_agent_without_verify() {
    let src = "[providers.p]\ncmd=\"true\"\n\
               [[task]]\nid=\"a\"\nkind=\"agent\"\nprompt=\"hi\"\nprovider=\"p\"\n";
    let v = validate(&parse_graph(src).unwrap());
    assert!(v.errors.is_empty(), "{:?}", v.errors);
    assert!(
        v.warnings
            .contains(&Warning::UnsupervisedAgentWithoutVerify("a".into())),
        "{:?}",
        v.warnings
    );
}

#[test]
fn warns_when_cost_cap_has_no_adapter() {
    let src = "[providers.p]\ncmd=\"true\"\n\
               [[task]]\nid=\"a\"\nkind=\"agent\"\nprompt=\"hi\"\nprovider=\"p\"\n\
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
               [[task]]\nid=\"a\"\nkind=\"agent\"\nprompt=\"hi\"\nprovider=\"p\"\n\
               verify=\"true\"\nmax_cost_usd=5.0\n";
    let v = validate(&parse_graph(src).unwrap());
    assert!(v.warnings.is_empty(), "{:?}", v.warnings);
}

#[test]
fn rejects_invalid_max_duration() {
    let src = "[[task]]\nid=\"a\"\nkind=\"shell\"\nrun=\"true\"\nmax_duration=\"soon\"\n";
    let v = validate(&parse_graph(src).unwrap());
    assert!(
        v.errors
            .iter()
            .any(|e| matches!(e, ValidationError::InvalidDuration { .. })),
        "{:?}",
        v.errors
    );
}
