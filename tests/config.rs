use assembly_line::config::{ConfigError, RepoConfig, Warning, parse_duration};
use std::time::Duration;

const FULL: &str = r#"
provider = "claude"
verify = "cargo test"
base = "develop"
max_duration = "20m"
copy = [".env", ".claude/settings.local.json"]

[delivery]
mode = "none"

[providers.claude]
cmd = "claude"
args = ["-p", "{prompt}"]
"#;

#[test]
fn parses_everything_a_repository_can_declare() {
    let config = RepoConfig::parse(FULL).expect("should parse");

    assert_eq!(config.provider.as_deref(), Some("claude"));
    assert_eq!(config.verify.as_deref(), Some("cargo test"));
    assert_eq!(config.base.as_deref(), Some("develop"));
    assert_eq!(config.max_duration.as_deref(), Some("20m"));
    assert_eq!(config.copy.len(), 2);
    assert_eq!(config.providers["claude"].cmd, "claude");
    assert_eq!(config.providers["claude"].args, vec!["-p", "{prompt}"]);
}

#[test]
fn an_empty_config_is_valid_and_declares_nothing() {
    let config = RepoConfig::parse("").unwrap();

    assert_eq!(config, RepoConfig::default());
}

#[test]
fn rejects_unknown_fields() {
    let err = RepoConfig::parse("provider = \"p\"\nnope = 1\n")
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

// Validation. There are no task ids any more — job ids are integers
// assembly-line allocates — so what is left to get wrong is the provider and
// the duration.

#[test]
fn a_provider_the_repository_never_declared_is_rejected() {
    let config = RepoConfig::parse("[providers.real]\ncmd = \"true\"\n").unwrap();

    assert_eq!(
        config.reasons_it_cannot_run("ghost"),
        vec![ConfigError::UnknownProvider("ghost".into())]
    );
}

#[test]
fn a_repository_naming_no_provider_at_all_is_rejected() {
    let config = RepoConfig::parse("[providers.real]\ncmd = \"true\"\n").unwrap();

    assert_eq!(
        config.reasons_it_cannot_run(""),
        vec![ConfigError::NoProviderDeclared]
    );
}

#[test]
fn a_declared_provider_is_accepted() {
    let config = RepoConfig::parse(
        "provider = \"real\"\nverify = \"true\"\n[providers.real]\ncmd = \"true\"\n",
    )
    .unwrap();

    assert!(config.reasons_it_cannot_run("real").is_empty());
}

#[test]
fn rejects_an_unparseable_max_duration() {
    let config = RepoConfig::parse(
        "provider = \"p\"\nmax_duration = \"soon\"\n[providers.p]\ncmd = \"true\"\n",
    )
    .unwrap();

    assert_eq!(
        config.reasons_it_cannot_run("p"),
        vec![ConfigError::UnparseableMaxDuration("soon".into())]
    );
}

#[test]
fn reports_every_problem_at_once() {
    let config = RepoConfig::parse("max_duration = \"soon\"\n").unwrap();

    assert_eq!(
        config.reasons_it_cannot_run(""),
        vec![
            ConfigError::NoProviderDeclared,
            ConfigError::UnparseableMaxDuration("soon".into())
        ],
        "a user should fix every problem in one pass, not one per run"
    );
}

#[test]
fn every_config_error_says_what_to_do_about_it() {
    assert!(
        ConfigError::UnknownProvider("ghost".into())
            .to_string()
            .contains("add a block for it")
    );
    assert!(
        ConfigError::NoProviderDeclared
            .to_string()
            .contains("[providers]")
    );
    assert!(
        ConfigError::UnparseableMaxDuration("soon".into())
            .to_string()
            .contains("20m")
    );
}

#[test]
fn a_repository_with_no_verify_is_warned_about_but_still_runnable() {
    let config = RepoConfig::parse("provider = \"p\"\n[providers.p]\ncmd = \"true\"\n").unwrap();

    assert!(config.reasons_it_cannot_run("p").is_empty());
    assert_eq!(config.settings_worth_flagging(), vec![Warning::NoVerify]);
    assert!(
        config.settings_worth_flagging()[0]
            .to_string()
            .contains("nothing will check")
    );
}

#[test]
fn a_repository_that_declares_verify_warns_about_nothing() {
    let config = RepoConfig::parse(
        "provider = \"p\"\nverify = \"cargo test\"\n[providers.p]\ncmd = \"true\"\n",
    )
    .unwrap();

    assert!(config.settings_worth_flagging().is_empty());
}
