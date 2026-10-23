use assembly_line::config::{ConfigError, DeliveryMode, RepoConfig, Warning, parse_duration};
use std::time::Duration;

const FULL: &str = r#"
provider = "claude"
verify = "cargo test"
base = "develop"
max_duration = "20m"

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

// Validation. Job ids are allocated, never configured, so what is left to get
// wrong is the provider and the duration.

fn reasons_it_cannot_run(body: &str, provider_named: Option<&str>) -> Vec<ConfigError> {
    RepoConfig::parse(body)
        .unwrap()
        .settings_for(provider_named)
        .expect_err("the settings were valid")
}

#[test]
fn a_provider_the_repository_never_declared_is_rejected() {
    assert_eq!(
        reasons_it_cannot_run("[providers.real]\ncmd = \"true\"\n", Some("ghost")),
        vec![ConfigError::UnknownProvider("ghost".into())]
    );
}

#[test]
fn a_repository_naming_no_provider_at_all_is_rejected() {
    assert_eq!(
        reasons_it_cannot_run("[providers.real]\ncmd = \"true\"\n", None),
        vec![ConfigError::NoProviderDeclared]
    );
}

#[test]
fn a_job_runs_under_the_provider_the_repository_names_by_default() {
    let settings = RepoConfig::parse(FULL).unwrap().settings_for(None).unwrap();

    assert_eq!(settings.provider_name, "claude");
    assert_eq!(settings.provider.cmd, "claude");
}

#[test]
fn a_provider_named_for_the_job_wins_over_the_default() {
    let config = RepoConfig::parse(
        "provider = \"real\"\n[providers.real]\ncmd = \"real\"\n[providers.other]\ncmd = \"other\"\n",
    )
    .unwrap();

    let settings = config.settings_for(Some("other")).unwrap();

    assert_eq!(settings.provider_name, "other");
    assert_eq!(settings.provider.cmd, "other");
}

#[test]
fn settings_carry_everything_else_the_repository_declares() {
    let settings = RepoConfig::parse(FULL).unwrap().settings_for(None).unwrap();

    assert_eq!(settings.verify.as_deref(), Some("cargo test"));
    assert_eq!(settings.pull_request_target.as_deref(), Some("develop"));
    assert_eq!(settings.delivery.mode, DeliveryMode::None);
}

#[test]
fn rejects_an_unparseable_max_duration() {
    assert_eq!(
        reasons_it_cannot_run(
            "provider = \"p\"\nmax_duration = \"soon\"\n[providers.p]\ncmd = \"true\"\n",
            None
        ),
        vec![ConfigError::UnparseableMaxDuration("soon".into())]
    );
}

#[test]
fn a_max_duration_reaches_the_settings_as_a_duration() {
    let max_duration = |body: &str| {
        RepoConfig::parse(body)
            .unwrap()
            .settings_for(Some("p"))
            .unwrap()
            .max_duration
    };
    let runnable = "[providers.p]\ncmd = \"true\"\n";

    assert_eq!(
        max_duration(&format!("max_duration = \"20m\"\n{runnable}")),
        Some(Duration::from_mins(20))
    );
    assert_eq!(max_duration(runnable), None);
}

#[test]
fn reports_every_problem_at_once() {
    assert_eq!(
        reasons_it_cannot_run("max_duration = \"soon\"\n", None),
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
    assert!(ConfigError::CopyRetired.to_string().contains("--pass-env"));
}

#[test]
fn a_config_that_still_declares_copy_is_told_what_to_do_instead() {
    let problems = reasons_it_cannot_run(
        "provider = \"p\"\ncopy = [\".env\"]\n[providers.p]\ncmd = \"p\"\n",
        None,
    );

    assert_eq!(problems, [ConfigError::CopyRetired]);
    let said = problems[0].to_string();
    assert!(
        said.contains("commit") && said.contains("--pass-env"),
        "{said}"
    );
}

#[test]
fn a_repository_with_no_verify_is_warned_about_but_still_runnable() {
    let settings = RepoConfig::parse("provider = \"p\"\n[providers.p]\ncmd = \"true\"\n")
        .unwrap()
        .settings_for(None)
        .expect("no verify is still runnable");

    assert_eq!(settings.settings_worth_flagging(), vec![Warning::NoVerify]);
    assert!(
        settings.settings_worth_flagging()[0]
            .to_string()
            .contains("nothing will check")
    );
}

#[test]
fn a_repository_that_declares_verify_warns_about_nothing() {
    let settings = RepoConfig::parse(
        "provider = \"p\"\nverify = \"cargo test\"\n[providers.p]\ncmd = \"true\"\n",
    )
    .unwrap()
    .settings_for(None)
    .unwrap();

    assert!(settings.settings_worth_flagging().is_empty());
}
