use assembly_line::config::{JobSettings, RepoConfig};
use assembly_line::daemon::api::{Queued, Refused, Submission};
use assembly_line::daemon::dispatch::{JobQueue, RepoLocks};
use assembly_line::daemon::submit::accept;
use assembly_line::daemon::{Daemon, Serving};
use assembly_line::git::PinnedRef;
use assembly_line::paths::JobPaths;
use assembly_line::payload::{FORGE_TOKEN_VAR, GIT_TOKEN_VAR};
use assembly_line::remote::Remote;
use assembly_line::report::{JobRequests, RoundRequest};
use assembly_line::runner::docker::{DockerRunner, docker_create_args};
use assembly_line::runner::kubernetes::KubernetesRunner;
use assembly_line::runner::local::{LocalRound, LocalRunner};
use assembly_line::runner::{
    JobSecrets, LaunchSpec, Reach, RoundEnvironment, RoundHandle, Runner, RunnerProblem,
};
use chrono::Utc;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

/// A runner that reports `problems` and never launches, running rounds
/// where `R` runs them.
#[derive(Debug)]
struct RunnerReporting<R> {
    problems: Vec<RunnerProblem>,
    runs_rounds_like: PhantomData<R>,
}

fn reporting<R>(problems: Vec<RunnerProblem>) -> RunnerReporting<R> {
    RunnerReporting {
        problems,
        runs_rounds_like: PhantomData,
    }
}

impl<R: Runner> Runner for RunnerReporting<R> {
    type Running = LocalRound;
    const ENVIRONMENT: RoundEnvironment = R::ENVIRONMENT;

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

    fn reattach(
        &self,
        _handle: &RoundHandle,
    ) -> impl Future<Output = anyhow::Result<LocalRound>> + Send {
        std::future::ready(Err(anyhow::anyhow!("a preflight test reattaches nothing")))
    }
}

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
fn a_round_that_reaches_only_network_remotes_cannot_reach_a_path_on_this_machine() {
    let reasons = |url| Reach::FromAnywhere.reasons_it_cannot_reach(&Remote::from_url(url));
    for url in ["/tmp/origin.git", "../origin", "file:///tmp/origin.git"] {
        assert_eq!(
            reasons(url),
            [RunnerProblem::RemoteIsLocalPath { url: url.into() }],
            "{url}"
        );
    }
    for url in [
        NETWORK_REMOTE,
        "git@github.com:o/r.git",
        "ssh://git@github.com/o/r.git",
    ] {
        assert_eq!(reasons(url), [], "{url}");
    }
}

async fn daemon_on<R: Runner>(
    runner: R,
    pass_env: &[&str],
    host: impl Fn(&str) -> Option<String>,
) -> Result<Daemon<R>, Vec<RunnerProblem>> {
    let pass_env: Vec<String> = pass_env.iter().map(ToString::to_string).collect();
    Daemon::prepare(PathBuf::from("/unused"), runner, 1, &pass_env, host).await
}

/// What a daemon on `R` sends each round from a host that has every
/// variable, when `--pass-env` names `ANTHROPIC_API_KEY`.
async fn secrets_sent_on<R: Runner>() -> Vec<String> {
    daemon_on(reporting::<R>(Vec::new()), &["ANTHROPIC_API_KEY"], |name| {
        Some(format!("value of {name}"))
    })
    .await
    .unwrap()
    .secrets()
    .names()
    .into_iter()
    .collect()
}

/// A round on the host has the host's own credentials; a round in a
/// container has only what is sent to it.
#[tokio::test]
async fn only_a_container_runner_sends_secrets() {
    let sent = ["ANTHROPIC_API_KEY", GIT_TOKEN_VAR, FORGE_TOKEN_VAR];
    assert_eq!(secrets_sent_on::<LocalRunner>().await, [] as [&str; 0]);
    assert_eq!(secrets_sent_on::<DockerRunner>().await, sent);
    assert_eq!(secrets_sent_on::<KubernetesRunner>().await, sent);
}

#[tokio::test]
async fn a_host_runner_is_refused_for_its_own_problems() {
    let problems = daemon_on(
        reporting::<LocalRunner>(vec![unreachable_runner()]),
        &[],
        |_| None,
    )
    .await
    .unwrap_err();

    assert_eq!(problems, [unreachable_runner()]);
}

/// How a daemon on `R` answers a submit whose remote is a path on this
/// machine that holds no repository.
async fn answer_to_a_local_path_on<R: Runner>() -> Result<Queued, Refused> {
    let root = tempfile::tempdir().unwrap();
    let serving = Serving {
        daemon: Daemon::prepare(
            root.path().to_path_buf(),
            reporting::<R>(Vec::new()),
            1,
            &[],
            |name| Some(format!("value of {name}")),
        )
        .await
        .unwrap(),
        repos: RepoLocks::default(),
        queue: JobQueue::new().0,
    };
    accept(
        &serving,
        Submission {
            remote_url: "/nonexistent/origin.git".into(),
            base_ref: Some("main".into()),
            job: None,
            prompt: "x".into(),
            provider: None,
        },
    )
    .await
}

/// A container cannot see the host's paths, so a container runner refuses
/// the remote before looking at it; the host runner goes on to find that
/// nothing is there.
#[tokio::test]
async fn only_a_container_runner_refuses_a_remote_that_is_a_local_path() {
    let for_being_a_local_path = Refused {
        summary: "the job cannot run on this daemon's runner".into(),
        reasons: vec![
            RunnerProblem::RemoteIsLocalPath {
                url: "/nonexistent/origin.git".into(),
            }
            .to_string(),
        ],
    };

    let local = answer_to_a_local_path_on::<LocalRunner>()
        .await
        .unwrap_err();
    assert_ne!(local.summary, for_being_a_local_path.summary, "{local:?}");
    assert_eq!(
        answer_to_a_local_path_on::<DockerRunner>()
            .await
            .unwrap_err(),
        for_being_a_local_path
    );
    assert_eq!(
        answer_to_a_local_path_on::<KubernetesRunner>()
            .await
            .unwrap_err(),
        for_being_a_local_path
    );
}

#[tokio::test]
async fn every_reason_a_container_runner_cannot_run_is_reported_at_once() {
    let problems = daemon_on(
        reporting::<DockerRunner>(vec![unreachable_runner()]),
        &["ANTHROPIC_API_KEY"],
        host_with_both_tokens,
    )
    .await
    .unwrap_err();

    assert_eq!(
        problems,
        [
            unreachable_runner(),
            RunnerProblem::MissingEnvironment("ANTHROPIC_API_KEY".into()),
        ]
    );
}

fn base() -> PinnedRef {
    PinnedRef {
        name: "main".into(),
        sha: "a".repeat(40).parse().unwrap(),
    }
}

/// A job asked for once, so its first request is its latest.
fn requests(remote_url: &str, prompt: &str, provider: &str) -> JobRequests {
    let request = RoundRequest {
        remote_url: remote_url.into(),
        base: base(),
        prompt: prompt.into(),
        provider: provider.into(),
        requested_at: Utc::now(),
    };
    JobRequests {
        first: request.clone(),
        latest: request,
    }
}

fn settings(max_duration: Option<&str>) -> JobSettings {
    let limit = max_duration
        .map(|limit| format!("max_duration = \"{limit}\"\n"))
        .unwrap_or_default();
    RepoConfig::parse(&format!(
        "provider = \"p\"\n{limit}[providers.p]\ncmd = \"true\"\n"
    ))
    .unwrap()
    .settings_for(None)
    .unwrap()
}

fn job(id: u64) -> JobPaths {
    JobPaths {
        id: id.into(),
        dir: PathBuf::from("/root/jobs/local/o").join(id.to_string()),
    }
}

/// A daemon restarted with another `--runner` finds its root's rounds but
/// cannot collect them, and says which runner launched them.
#[tokio::test]
async fn a_round_launched_by_another_runner_is_not_reattached() {
    let docker_round = RoundHandle::Docker {
        container: "al-1-1-x".into(),
    };
    let local_round = RoundHandle::Local {
        pid: 1,
        frames: job(1).frames(1),
    };

    let local = LocalRunner::using("assembly")
        .reattach(&docker_round)
        .await
        .unwrap_err()
        .to_string();
    let docker = DockerRunner::new("img:1".into())
        .reattach(&local_round)
        .await
        .unwrap_err()
        .to_string();

    assert!(
        local.contains("launched by a docker runner") && local.contains("runs local"),
        "{local}"
    );
    assert!(
        docker.contains("launched by a local runner") && docker.contains("runs docker"),
        "{docker}"
    );
}

#[test]
fn a_container_round_runs_assembly_run_over_https_and_provisions_first() {
    let spec = LaunchSpec::for_round::<DockerRunner>(
        &job(7),
        2,
        &requests("git@github.com:o/r.git", "fix it", "claude"),
        &base(),
        &settings(Some("1m")),
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
    assert_eq!(
        spec.frames_file,
        Path::new("/root/jobs/local/o/7/round-2.frames")
    );
}

fn provisions_the_toolchain<R: Runner>() -> bool {
    LaunchSpec::for_round::<R>(
        &job(1),
        1,
        &requests(NETWORK_REMOTE, "x", "p"),
        &base(),
        &settings(None),
    )
    .args
    .contains(&"--provision-toolchain".to_string())
}

/// A round on the host uses the host's toolchain; a container brings none.
#[test]
fn only_a_container_runner_provisions_the_toolchain() {
    assert!(!provisions_the_toolchain::<LocalRunner>());
    assert!(provisions_the_toolchain::<DockerRunner>());
    assert!(provisions_the_toolchain::<KubernetesRunner>());
}

/// A revise records the URL as it was typed, which can be another spelling
/// of the job's remote; the job still pushes where its first request said.
#[test]
fn a_revised_round_runs_against_the_first_remote_with_the_latest_prompt() {
    let revised = JobRequests {
        latest: RoundRequest {
            remote_url: "ssh://git@github.com/o/r".into(),
            prompt: "now the tests".into(),
            provider: "codex".into(),
            ..requests("git@github.com:o/r.git", "x", "claude").latest
        },
        ..requests("git@github.com:o/r.git", "fix it", "claude")
    };

    let spec = LaunchSpec::for_round::<LocalRunner>(&job(7), 2, &revised, &base(), &settings(None));

    assert_eq!(
        spec.args[1..],
        [
            "--repo=git@github.com:o/r.git".to_string(),
            format!("--ref=main@{}", "a".repeat(40)),
            "--job=7".into(),
            "--prompt=now the tests".into(),
            "--provider=codex".into(),
            "--frames".into(),
        ]
    );
}

/// Review focus 1, at the seam: every value rides after `=`, so no prompt
/// can be mistaken for a flag.
#[test]
fn every_value_on_the_command_line_is_attached_to_its_flag() {
    let spec = LaunchSpec::for_round::<LocalRunner>(
        &job(1),
        1,
        &requests("/o.git", "--frames\n\"quoted\"", "p"),
        &base(),
        &settings(None),
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
fn docker_create_names_its_secrets_and_runs_assembly_run() {
    let args = docker_create_args(
        "img:1",
        "al-1-1-x",
        &["ASSEMBLY_GIT_TOKEN", "GH_TOKEN"],
        &["run".into(), "--job=1".into()],
    );
    let joined = args.join(" ");

    assert!(
        joined.starts_with("create --name al-1-1-x -e ASSEMBLY_GIT_TOKEN -e GH_TOKEN"),
        "{joined}"
    );
    assert!(joined.ends_with("img:1 assembly run --job=1"), "{joined}");
}
