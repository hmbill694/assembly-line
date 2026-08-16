use assembly_line::config::{OnFailure, Supervise, TaskKind, parse_duration, parse_graph};
use std::time::Duration;

const FULL: &str = r#"
[workspace]
copy = [".env"]

[providers.claude]
cmd = "claude"
args = ["-p", "{prompt}"]

[[task]]
id = "build"
kind = "shell"
run = "cargo build"

[[task]]
id = "impl-auth"
kind = "agent"
needs = ["build"]
prompt = "do the thing"
provider = "claude"
verify = "cargo test"
supervise = "on-complete"
retries = 2
max_duration = "20m"
resource = "postgres"
on_failure = "abort"

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
    assert_eq!(g.tasks[0].kind, TaskKind::Shell);
    assert_eq!(g.tasks[0].run.as_deref(), Some("cargo build"));

    let agent = &g.tasks[1];
    assert_eq!(agent.kind, TaskKind::Agent);
    assert_eq!(agent.needs, vec!["build".to_string()]);
    assert_eq!(agent.provider.as_deref(), Some("claude"));
    assert_eq!(agent.supervise, Supervise::OnComplete);
    assert_eq!(agent.on_failure, OnFailure::Abort);
    assert_eq!(agent.retries, 2);
    assert_eq!(agent.resource.as_deref(), Some("postgres"));
    assert_eq!(agent.max_duration.as_deref(), Some("20m"));
}

#[test]
fn applies_defaults() {
    let g = parse_graph("[[task]]\nid = \"a\"\nkind = \"shell\"\nrun = \"true\"\n").unwrap();
    let t = &g.tasks[0];
    assert!(t.needs.is_empty());
    assert!(t.copy.is_empty());
    assert_eq!(t.supervise, Supervise::None);
    assert_eq!(t.on_failure, OnFailure::Skip);
    assert_eq!(t.retries, 0);
    assert!(t.max_cost_usd.is_none());
}

#[test]
fn rejects_unknown_fields() {
    let err = parse_graph("[[task]]\nid = \"a\"\nkind = \"shell\"\nrun = \"true\"\nnope = 1\n")
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
