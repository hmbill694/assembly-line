use assembly_line::config::{ValidationError, Warning, parse_duration, parse_graph, validate};
use std::time::Duration;

const FULL: &str = r#"
[workspace]
copy = [".env"]

[providers.claude]
cmd = "claude"
args = ["-p", "{prompt}"]

[[task]]
id = "build"
prompt = "cargo build"

[[task]]
id = "impl-auth"
prompt = "do the thing"
provider = "claude"
verify = "cargo test"
retries = 2
max_duration = "20m"

[[hook]]
on = "run_complete"
when = "success"
run = "echo done"
"#;

#[test]
fn parses_a_full_graph() {
    let g = parse_graph(FULL).expect("should parse");

    assert_eq!(g.workspace.copy, vec![".env".to_string()]);
    assert_eq!(g.providers["claude"].cmd, "claude");
    assert_eq!(g.providers["claude"].args, vec!["-p", "{prompt}"]);
    assert_eq!(g.hooks.len(), 1);
    assert_eq!(g.hooks[0].on, "run_complete");

    assert_eq!(g.tasks.len(), 2);
    assert_eq!(g.tasks[0].prompt.as_deref(), Some("cargo build"));

    let agent = &g.tasks[1];
    assert_eq!(agent.provider.as_deref(), Some("claude"));
    assert_eq!(agent.retries, 2);
    assert_eq!(agent.max_duration.as_deref(), Some("20m"));
}

#[test]
fn applies_defaults() {
    let g = parse_graph("[[task]]\nid = \"a\"\nprompt = \"x\"\n").unwrap();
    let t = &g.tasks[0];
    assert!(t.copy.is_empty());
    assert_eq!(t.retries, 0);
    assert!(t.max_cost_usd.is_none());
}

#[test]
fn rejects_unknown_fields() {
    let err = parse_graph("[[task]]\nid = \"a\"\nprompt = \"x\"\nnope = 1\n")
        .expect_err("unknown field must be rejected");
    assert!(err.to_string().contains("nope"), "got: {err}");
}

#[test]
#[allow(
    clippy::duration_suboptimal_units,
    reason = "seconds are the point: the assertion shows what the text converts to"
)]
fn parses_durations() {
    assert_eq!(parse_duration("20m").unwrap(), Duration::from_secs(1200));
    assert_eq!(parse_duration("1h 30m").unwrap(), Duration::from_secs(5400));
    assert!(parse_duration("soon").is_err());
}

// Validation: tasks have no dependencies to order any more, but an id still
// becomes a filename and a branch name, and an agent still needs a prompt and
// a real provider. These checks survive even though task ids themselves are
// on their way out — this revision's tree still has them, and still must
// validate them.

#[test]
fn rejects_duplicate_ids() {
    let graph = parse_graph(
        "[[task]]\nid=\"a\"\nprompt=\"x\"\n\
         [[task]]\nid=\"a\"\nprompt=\"x\"\n",
    )
    .unwrap();
    let errs = validate(&graph).errors;
    assert!(
        errs.contains(&ValidationError::DuplicateId("a".into())),
        "{errs:?}"
    );
}

#[test]
fn an_id_that_cannot_be_a_branch_name_is_rejected() {
    let graph = parse_graph(
        r#"
        [[task]]
        id = "impl auth"
        prompt = "go"
        "#,
    )
    .unwrap();

    assert!(
        validate(&graph)
            .errors
            .iter()
            .any(|e| matches!(e, ValidationError::InvalidId(id) if id == "impl auth")),
    );
}

#[test]
fn rejects_an_id_that_starts_with_the_reserved_underscore_prefix() {
    let graph = parse_graph("[[task]]\nid=\"_scratch\"\nprompt=\"x\"\n").unwrap();
    let errs = validate(&graph).errors;
    assert!(
        errs.contains(&ValidationError::ReservedId("_scratch".into())),
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
fn rejects_unknown_provider() {
    let graph = parse_graph("[[task]]\nid=\"a\"\nprompt=\"hi\"\nprovider=\"nope\"\n").unwrap();
    let v = validate(&graph);
    assert!(
        v.errors.contains(&ValidationError::UnknownProvider {
            task: "a".into(),
            provider: "nope".into()
        }),
        "{:?}",
        v.errors
    );
}

#[test]
fn rejects_invalid_max_duration() {
    let graph = parse_graph("[[task]]\nid=\"a\"\nprompt=\"x\"\nmax_duration=\"soon\"\n").unwrap();
    let v = validate(&graph);
    assert!(
        v.errors
            .iter()
            .any(|e| matches!(e, ValidationError::InvalidDuration { .. })),
        "{:?}",
        v.errors
    );
}

#[test]
fn reports_every_problem_at_once() {
    let graph = parse_graph(
        "[[task]]\nid=\"a\"\n\
         [[task]]\nid=\"b\"\nprompt=\"x\"\nprovider=\"ghost\"\n",
    )
    .unwrap();
    let errs = validate(&graph).errors;
    assert_eq!(errs.len(), 2, "{errs:?}");
}

#[test]
fn warns_on_agent_without_verify() {
    let graph = parse_graph(
        "[providers.p]\ncmd=\"true\"\n\
         [[task]]\nid=\"a\"\nprompt=\"hi\"\nprovider=\"p\"\n",
    )
    .unwrap();
    let v = validate(&graph);
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
    let graph = parse_graph(
        "[providers.p]\ncmd=\"true\"\n\
         [[task]]\nid=\"a\"\nprompt=\"hi\"\nprovider=\"p\"\n\
         verify=\"true\"\nmax_cost_usd=5.0\n",
    )
    .unwrap();
    let v = validate(&graph);
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
    let graph = parse_graph(
        "[providers.p]\ncmd=\"true\"\nadapter=\"wrap.sh\"\n\
         [[task]]\nid=\"a\"\nprompt=\"hi\"\nprovider=\"p\"\n\
         verify=\"true\"\nmax_cost_usd=5.0\n",
    )
    .unwrap();
    let v = validate(&graph);
    assert!(v.warnings.is_empty(), "{:?}", v.warnings);
}
