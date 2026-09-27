use assembly_line::payload::{
    GIT_TOKEN_VAR, PAYLOAD_VAR, RoundPayload, https_equivalent, is_path_on_this_machine,
};
use assembly_line::runner::docker::docker_run_args;
use assembly_line::runner::local::LocalRound;
use assembly_line::runner::{
    JobSecrets, Runner, RunnerProblem, reasons_a_container_cannot_run,
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
        _payload: &RoundPayload,
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

fn host_with_only_the_git_token(name: &str) -> Option<String> {
    (name == GIT_TOKEN_VAR).then(|| "t0ken".to_string())
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
fn a_container_always_receives_the_git_token_and_only_the_named_extras() {
    let host = |name: &str| match name {
        "ASSEMBLY_GIT_TOKEN" => Some("t0ken".to_string()),
        "ANTHROPIC_API_KEY" => Some("sk".to_string()),
        "AWS_SECRET_ACCESS_KEY" => Some("never".to_string()),
        _ => None,
    };

    let (secrets, problems) = JobSecrets::from_lookup(&["ANTHROPIC_API_KEY".into()], host);

    assert!(problems.is_empty(), "{problems:?}");
    // Sorted: `names` is a set.
    assert_eq!(
        secrets.names().into_iter().collect::<Vec<_>>(),
        ["ANTHROPIC_API_KEY", GIT_TOKEN_VAR]
    );
}

#[test]
fn every_missing_variable_is_reported_at_once() {
    let (_, problems) = JobSecrets::from_lookup(&["ANTHROPIC_API_KEY".into()], |_| None);
    assert_eq!(
        problems,
        [
            RunnerProblem::MissingEnvironment(GIT_TOKEN_VAR.into()),
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
            PAYLOAD_VAR.into(),
            PAYLOAD_VAR.into(),
        ],
        |name| (name == GIT_TOKEN_VAR).then(|| "t".to_string()),
    );

    assert_eq!(
        problems,
        [
            RunnerProblem::ReservedEnvironment(PAYLOAD_VAR.into()),
            RunnerProblem::MissingEnvironment("X".into()),
        ]
    );
}

/// `ASSEMBLY_JOB` would override the payload itself; the git token is sent
/// whatever `--pass-env` says.
#[test]
fn passing_a_variable_assembly_line_sets_itself_is_refused() {
    let (secrets, problems) = JobSecrets::from_lookup(
        &[PAYLOAD_VAR.into(), GIT_TOKEN_VAR.into(), "EXTRA".into()],
        |name| Some(format!("value of {name}")),
    );

    assert_eq!(
        problems,
        [
            RunnerProblem::ReservedEnvironment(PAYLOAD_VAR.into()),
            RunnerProblem::ReservedEnvironment(GIT_TOKEN_VAR.into()),
        ]
    );
    assert_eq!(
        secrets.names().into_iter().collect::<Vec<_>>(),
        [GIT_TOKEN_VAR, "EXTRA"]
    );
}

const NETWORK_REMOTE: &str = "https://github.com/o/r.git";

#[test]
fn a_repository_that_declares_copy_cannot_run_in_a_container() {
    assert_eq!(
        reasons_a_container_cannot_run(&[".env".into()], NETWORK_REMOTE),
        [RunnerProblem::CopyNeedsLocalRunner]
    );
    assert!(reasons_a_container_cannot_run(&[], NETWORK_REMOTE).is_empty());
}

#[test]
fn a_remote_that_is_a_path_on_this_machine_cannot_run_in_a_container() {
    for url in ["/tmp/origin.git", "../origin", "file:///tmp/origin.git"] {
        assert_eq!(
            reasons_a_container_cannot_run(&[], url),
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

#[test]
fn every_container_problem_is_reported_at_once() {
    assert_eq!(
        reasons_a_container_cannot_run(&[".env".into()], "/tmp/origin.git"),
        [
            RunnerProblem::CopyNeedsLocalRunner,
            RunnerProblem::RemoteIsLocalPath {
                url: "/tmp/origin.git".into()
            },
        ]
    );
}

/// The host's own checkout, remote and credentials are all there for a
/// round that runs beside them.
#[tokio::test]
async fn a_host_runner_carries_no_secrets_and_takes_what_a_container_cannot() {
    let secrets = secrets_or_reasons_it_cannot_run(
        &HostRunner {
            problems: Vec::new(),
        },
        &[".env".into()],
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
        &[],
        NETWORK_REMOTE,
        &[],
        |_| None,
    )
    .await
    .unwrap_err();

    assert_eq!(problems, [unreachable_runner()]);
}

#[tokio::test]
async fn a_container_runner_carries_the_git_token_from_the_host() {
    let secrets = secrets_or_reasons_it_cannot_run(
        &ContainerRunner {
            problems: Vec::new(),
        },
        &[],
        NETWORK_REMOTE,
        &[],
        host_with_only_the_git_token,
    )
    .await
    .unwrap();

    assert_eq!(
        secrets.names().into_iter().collect::<Vec<_>>(),
        [GIT_TOKEN_VAR]
    );
}

#[tokio::test]
async fn every_reason_a_container_runner_cannot_run_is_reported_at_once() {
    let problems = secrets_or_reasons_it_cannot_run(
        &ContainerRunner {
            problems: vec![unreachable_runner()],
        },
        &[".env".into()],
        "/tmp/origin.git",
        &["ANTHROPIC_API_KEY".into()],
        host_with_only_the_git_token,
    )
    .await
    .unwrap_err();

    assert_eq!(
        problems,
        [
            unreachable_runner(),
            RunnerProblem::CopyNeedsLocalRunner,
            RunnerProblem::RemoteIsLocalPath {
                url: "/tmp/origin.git".into()
            },
            RunnerProblem::MissingEnvironment("ANTHROPIC_API_KEY".into()),
        ]
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
        RunnerProblem::CopyNeedsLocalRunner,
        RunnerProblem::MissingEnvironment("X".into()),
        RunnerProblem::ReservedEnvironment("ASSEMBLY_JOB".into()),
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
fn docker_run_names_its_secrets_and_runs_job_exec() {
    let args = docker_run_args(
        "img:1",
        "al-1-1-abc",
        &["ASSEMBLY_JOB", "ASSEMBLY_GIT_TOKEN"],
    );
    let joined = args.join(" ");

    assert!(joined.contains("-e ASSEMBLY_JOB"), "{joined}");
    assert!(joined.contains("-e ASSEMBLY_GIT_TOKEN"), "{joined}");
    assert!(joined.ends_with("img:1 assembly job-exec"), "{joined}");
}
