use assembly_line::git::PinnedRef;
use assembly_line::job::JobId;
use assembly_line::payload::{
    FORGE_TOKEN_VAR, GIT_TOKEN_VAR, https_equivalent, is_path_on_this_machine,
};
use assembly_line::runner::docker::docker_run_args;
use assembly_line::runner::local::LocalRound;
use assembly_line::runner::{
    JobSecrets, LaunchSpec, Runner, RunnerProblem, reasons_a_container_cannot_run,
    secrets_or_reasons_it_cannot_run,
};
use tokio_util::sync::CancellationToken;

/// A runner that reports `problems` and never launches, running rounds in a
/// container or not as `IN_A_CONTAINER` says.
struct RunnerReporting<const IN_A_CONTAINER: bool> {
    problems: Vec<RunnerProblem>,
}

impl<const IN_A_CONTAINER: bool> Runner for RunnerReporting<IN_A_CONTAINER> {
    type Running = LocalRound;
    const RUNS_IN_A_CONTAINER: bool = IN_A_CONTAINER;

    fn reasons_it_cannot_run(&self) -> impl Future<Output = Vec<RunnerProblem>> + Send {
        std::future::ready(self.problems.clone())
    }

    fn launch(
        &self,
        _spec: &LaunchSpec,
        _secrets: &JobSecrets,
        _cancel: &CancellationToken,
    ) -> impl Future<Output = anyhow::Result<LocalRound>> + Send {
        std::future::ready(Err(anyhow::anyhow!("a preflight test launches nothing")))
    }
}

type HostRunner = RunnerReporting<false>;
type ContainerRunner = RunnerReporting<true>;

fn unreachable_runner() -> RunnerProblem {
    RunnerProblem::Unreachable {
        runner: "docker",
        detail: "no daemon".into(),
    }
}

fn host_with_both_tokens(name: &str) -> Option<String> {
    [GIT_TOKEN_VAR, FORGE_TOKEN_VAR]
        .contains(&name)
        .then(|| "t0ken".to_string())
}

#[test]
fn ssh_remotes_become_the_https_url_a_token_can_authenticate() {
    assert_eq!(
        https_equivalent("git@github.com:o/r.git"),
        "https://github.com/o/r.git"
    );
    assert_eq!(
        https_equivalent("ssh://git@github.com/o/r.git"),
        "https://github.com/o/r.git"
    );
    assert_eq!(
        https_equivalent("ssh://git@github.com:22/o/r.git"),
        "https://github.com/o/r.git"
    );
    assert_eq!(
        https_equivalent("git@git.example.com:/srv/r.git"),
        "https://git.example.com/srv/r.git"
    );
}

/// An IPv6 literal's own colons are not the port's, nor scp-like's.
#[test]
fn an_ipv6_host_keeps_its_brackets() {
    assert_eq!(
        https_equivalent("ssh://git@[::1]/o/r.git"),
        "https://[::1]/o/r.git"
    );
    assert_eq!(
        https_equivalent("ssh://git@[::1]:22/o/r.git"),
        "https://[::1]/o/r.git"
    );
    assert_eq!(
        https_equivalent("git@[::1]:o/r.git"),
        "https://[::1]/o/r.git"
    );
}

#[test]
fn urls_a_token_already_works_with_are_left_alone() {
    for url in [
        "https://github.com/o/r.git",
        "/tmp/origin.git",
        "file:///tmp/origin.git",
        "../origin",
    ] {
        assert_eq!(https_equivalent(url), url);
    }
}

#[test]
fn a_container_always_receives_both_tokens_and_only_the_named_extras() {
    let host = |name: &str| match name {
        "ASSEMBLY_GIT_TOKEN" | "GH_TOKEN" => Some("t0ken".to_string()),
        "ANTHROPIC_API_KEY" => Some("sk".to_string()),
        "AWS_SECRET_ACCESS_KEY" => Some("never".to_string()),
        _ => None,
    };

    let (secrets, problems) = JobSecrets::from_lookup(&["ANTHROPIC_API_KEY".into()], host);

    assert!(problems.is_empty(), "{problems:?}");
    // Sorted: `names` is a set.
    assert_eq!(
        secrets.names().into_iter().collect::<Vec<_>>(),
        ["ANTHROPIC_API_KEY", GIT_TOKEN_VAR, FORGE_TOKEN_VAR]
    );
}

#[test]
fn every_missing_variable_is_reported_at_once() {
    let (_, problems) = JobSecrets::from_lookup(&["ANTHROPIC_API_KEY".into()], |_| None);
    assert_eq!(
        problems,
        [
            RunnerProblem::MissingEnvironment(GIT_TOKEN_VAR.into()),
            RunnerProblem::MissingEnvironment(FORGE_TOKEN_VAR.into()),
            RunnerProblem::MissingEnvironment("ANTHROPIC_API_KEY".into()),
        ]
    );
}

#[test]
fn a_secrets_debug_output_names_each_variable_and_shows_no_value() {
    let (secrets, _) = JobSecrets::from_lookup(&[], |_| Some("t0ken-value".to_string()));
    let printed = format!("{secrets:?}");

    assert!(printed.contains(GIT_TOKEN_VAR), "{printed}");
    assert!(
        !printed.contains("t0ken-value"),
        "a secret leaked: {printed}"
    );
}

#[test]
fn a_name_passed_twice_is_one_problem_not_two() {
    let (_, problems) = JobSecrets::from_lookup(
        &[
            "X".into(),
            "X".into(),
            FORGE_TOKEN_VAR.into(),
            FORGE_TOKEN_VAR.into(),
        ],
        host_with_both_tokens,
    );

    assert_eq!(
        problems,
        [
            RunnerProblem::ReservedEnvironment(FORGE_TOKEN_VAR.into()),
            RunnerProblem::MissingEnvironment("X".into()),
        ]
    );
}

/// Both tokens are sent whatever `--pass-env` says.
#[test]
fn passing_a_variable_assembly_line_sets_itself_is_refused() {
    let (secrets, problems) = JobSecrets::from_lookup(
        &[FORGE_TOKEN_VAR.into(), GIT_TOKEN_VAR.into(), "EXTRA".into()],
        |name| Some(format!("value of {name}")),
    );

    assert_eq!(
        problems,
        [
            RunnerProblem::ReservedEnvironment(FORGE_TOKEN_VAR.into()),
            RunnerProblem::ReservedEnvironment(GIT_TOKEN_VAR.into()),
        ]
    );
    assert_eq!(
        secrets.names().into_iter().collect::<Vec<_>>(),
        [GIT_TOKEN_VAR, "EXTRA", FORGE_TOKEN_VAR]
    );
}

const NETWORK_REMOTE: &str = "https://github.com/o/r.git";

#[test]
fn a_remote_that_is_a_path_on_this_machine_cannot_run_in_a_container() {
    for url in ["/tmp/origin.git", "../origin", "file:///tmp/origin.git"] {
        assert_eq!(
            reasons_a_container_cannot_run(url),
            [RunnerProblem::RemoteIsLocalPath { url: url.into() }],
            "{url}"
        );
    }
    for url in [
        NETWORK_REMOTE,
        "git@github.com:o/r.git",
        "ssh://git@github.com/o/r.git",
    ] {
        assert!(!is_path_on_this_machine(url), "{url}");
    }
}

/// The host's own checkout, remote and credentials are all there for a
/// round that runs beside them.
#[tokio::test]
async fn a_host_runner_carries_no_secrets_and_takes_what_a_container_cannot() {
    let secrets = secrets_or_reasons_it_cannot_run(
        &HostRunner {
            problems: Vec::new(),
        },
        "/tmp/origin.git",
        &["ANTHROPIC_API_KEY".into()],
        |_| None,
    )
    .await
    .unwrap();

    assert!(secrets.names().is_empty(), "{secrets:?}");
}

#[tokio::test]
async fn a_host_runner_is_refused_for_its_own_problems() {
    let problems = secrets_or_reasons_it_cannot_run(
        &HostRunner {
            problems: vec![unreachable_runner()],
        },
        NETWORK_REMOTE,
        &[],
        |_| None,
    )
    .await
    .unwrap_err();

    assert_eq!(problems, [unreachable_runner()]);
}

#[tokio::test]
async fn a_container_runner_carries_both_tokens_from_the_host() {
    let secrets = secrets_or_reasons_it_cannot_run(
        &ContainerRunner {
            problems: Vec::new(),
        },
        NETWORK_REMOTE,
        &[],
        host_with_both_tokens,
    )
    .await
    .unwrap();

    assert_eq!(
        secrets.names().into_iter().collect::<Vec<_>>(),
        [GIT_TOKEN_VAR, FORGE_TOKEN_VAR]
    );
}

#[tokio::test]
async fn every_reason_a_container_runner_cannot_run_is_reported_at_once() {
    let problems = secrets_or_reasons_it_cannot_run(
        &ContainerRunner {
            problems: vec![unreachable_runner()],
        },
        "/tmp/origin.git",
        &["ANTHROPIC_API_KEY".into()],
        host_with_both_tokens,
    )
    .await
    .unwrap_err();

    assert_eq!(
        problems,
        [
            unreachable_runner(),
            RunnerProblem::RemoteIsLocalPath {
                url: "/tmp/origin.git".into()
            },
            RunnerProblem::MissingEnvironment("ANTHROPIC_API_KEY".into()),
        ]
    );
}

fn base() -> PinnedRef {
    PinnedRef {
        name: "main".into(),
        sha: "a".repeat(40),
    }
}

#[test]
fn a_container_round_runs_assembly_run_over_https_and_provisions_first() {
    let spec = LaunchSpec::for_round::<ContainerRunner>(
        JobId::from(7),
        2,
        "git@github.com:o/r.git",
        &base(),
        "fix it",
        "claude",
        Some(60),
    );

    assert_eq!(
        spec.args,
        [
            "run".to_string(),
            "--repo=https://github.com/o/r.git".into(),
            format!("--ref=main@{}", "a".repeat(40)),
            "--job=7".into(),
            "--prompt=fix it".into(),
            "--provider=claude".into(),
            "--frames".into(),
            "--provision-toolchain".into(),
        ]
    );
    assert_eq!(spec.command_limit_secs, Some(60));
    assert!(spec.name.starts_with("al-7-2-"), "{}", spec.name);
}

#[test]
fn a_host_round_runs_assembly_run_with_the_remote_and_toolchain_as_they_are() {
    let spec = LaunchSpec::for_round::<HostRunner>(
        JobId::from(7),
        1,
        "git@github.com:o/r.git",
        &base(),
        "x",
        "claude",
        None,
    );

    assert!(
        spec.args
            .contains(&"--repo=git@github.com:o/r.git".to_string())
    );
    assert!(!spec.args.contains(&"--provision-toolchain".to_string()));
}

/// A remote that is a path could be read by `run` as a checkout whose own
/// remote is somewhere else; a URL cannot.
#[test]
fn a_host_round_names_a_remote_on_this_machine_by_its_file_url() {
    let spec = LaunchSpec::for_round::<HostRunner>(
        JobId::from(7),
        1,
        "/srv/origin",
        &base(),
        "x",
        "claude",
        None,
    );

    assert!(
        spec.args.contains(&"--repo=file:///srv/origin".to_string()),
        "{:?}",
        spec.args
    );
}

/// Review focus 1, at the seam: every value rides after `=`, so no prompt
/// can be mistaken for a flag.
#[test]
fn every_value_on_the_command_line_is_attached_to_its_flag() {
    let spec = LaunchSpec::for_round::<HostRunner>(
        JobId::from(1),
        1,
        "/o.git",
        &base(),
        "--frames\n\"quoted\"",
        "p",
        None,
    );

    assert!(
        spec.args
            .contains(&"--prompt=--frames\n\"quoted\"".to_string())
    );
}

#[test]
fn every_runner_problem_says_what_to_do_about_it() {
    let problems = [
        RunnerProblem::Unreachable {
            runner: "docker",
            detail: "no daemon".into(),
        },
        RunnerProblem::NotPermitted {
            verb: "create".into(),
            resource: "jobs".into(),
            namespace: "factory".into(),
        },
        RunnerProblem::MissingEnvironment("X".into()),
        RunnerProblem::ReservedEnvironment("GH_TOKEN".into()),
        RunnerProblem::RemoteIsLocalPath {
            url: "/tmp/origin.git".into(),
        },
    ];
    for problem in problems {
        let message = problem.to_string();
        assert!(message.contains(" — "), "no remedy in: {message}");
    }
}

#[test]
fn docker_run_names_its_secrets_and_runs_assembly_run() {
    let args = docker_run_args(
        "img:1",
        "al-1-1-x",
        &["ASSEMBLY_GIT_TOKEN", "GH_TOKEN"],
        &["run".into(), "--job=1".into()],
    );
    let joined = args.join(" ");

    assert!(
        joined.starts_with("run --name al-1-1-x -e ASSEMBLY_GIT_TOKEN -e GH_TOKEN"),
        "{joined}"
    );
    assert!(joined.ends_with("img:1 assembly run --job=1"), "{joined}");
}
