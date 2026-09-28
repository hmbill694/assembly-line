use assembly_line::collect::collect;
use assembly_line::event::{EventKind, EventLog};
use assembly_line::payload::{FORGE_TOKEN_VAR, GIT_TOKEN_VAR};
use assembly_line::runner::docker::DockerRunner;
use assembly_line::runner::{JobSecrets, Runner, RunnerProblem, RunningRound};
use std::path::{Path, PathBuf};
use support::{Harness, fake_cli};
use tokio_util::sync::CancellationToken;

mod support;

/// A `docker` that records its argv and, for `run`, executes the command it
/// was given after the image directly — the environment `docker run -e
/// NAME` would forward is already the environment this script inherits.
/// `stop` sends that `assembly run` SIGTERM, as the daemon would send the
/// container's.
fn fake_docker(dir: &Path, argv_log: &Path, oom: bool) -> PathBuf {
    fake_cli(
        dir,
        "docker",
        &format!(
            "echo \"$*\" >> {log}\n\
             case \"$1\" in\n\
               run) shift; while [ \"$1\" != assembly ]; do shift; done; shift\n\
                    echo $$ > {pid}; exec {bin} \"$@\" ;;\n\
               stop) kill -TERM \"$(cat {pid})\" ;;\n\
               version) echo 27.0.0 ;;\n\
               inspect) echo {oom} ;;\n\
               *) ;;\n\
             esac\n",
            log = argv_log.display(),
            pid = dir.join("run.pid").display(),
            bin = env!("CARGO_BIN_EXE_assembly"),
        ),
    )
}

fn token() -> JobSecrets {
    JobSecrets::from_lookup(&[], |name| {
        [GIT_TOKEN_VAR, FORGE_TOKEN_VAR]
            .contains(&name)
            .then(|| TOKEN_VALUE.to_string())
    })
    .0
}

const TOKEN_VALUE: &str = "token-value-never-in-argv";

#[tokio::test]
async fn a_round_in_docker_is_launched_collected_and_cleaned_up() {
    let h = Harness::new().await;
    let fakes = h.scratch_root().with_file_name("fakes");
    let argv = fakes.join("argv");
    let runner = DockerRunner {
        program: fake_docker(&fakes, &argv, false),
        image: "img:1".into(),
    };
    let spec = h.launch_spec_for("write a file").await;
    let paths = h.job_paths();
    let mut log = EventLog::open_append(paths.events()).unwrap();

    let running = runner
        .launch(&spec, &token(), &CancellationToken::new())
        .await
        .unwrap();
    let verdict = collect(running, &mut log, &paths.log(), CancellationToken::new())
        .await
        .unwrap();

    assert!(
        verdict.passed(),
        "{:?}",
        EventLog::read(paths.events()).unwrap()
    );
    let calls = std::fs::read_to_string(&argv).unwrap();
    assert!(calls.contains("run --name al-1-1-"), "{calls}");
    assert!(calls.contains("-e ASSEMBLY_GIT_TOKEN"), "{calls}");
    assert!(
        !calls.contains(TOKEN_VALUE),
        "a secret's value reached the command line: {calls}"
    );
    assert!(
        calls.lines().any(|l| l.starts_with("rm -f al-1-1-")),
        "the container was left behind: {calls}"
    );
}

/// `run` holds both tokens — the fake `docker` hands it the secrets the way
/// `-e` would — and the agent it starts sees neither in its own
/// environment.
#[tokio::test]
async fn the_agent_sees_neither_token() {
    let h = Harness::with_config(&support::config_running("env-reporting-agent.sh")).await;
    let fakes = h.scratch_root().with_file_name("fakes");
    let runner = DockerRunner {
        program: fake_docker(&fakes, &fakes.join("argv"), false),
        image: "img:1".into(),
    };
    let paths = h.job_paths();
    let mut log = EventLog::open_append(paths.events()).unwrap();

    let running = runner
        .launch(
            &h.launch_spec_for("x").await,
            &token(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    collect(running, &mut log, &paths.log(), CancellationToken::new())
        .await
        .unwrap();

    let output = std::fs::read_to_string(paths.log()).unwrap();
    assert!(output.contains("token=absent forge=absent"), "{output}");
}

/// A collector that cannot write its logs gives up — but not before stopping
/// the container, which would otherwise run on uncollected.
#[tokio::test]
async fn a_collector_that_cannot_write_removes_the_container_before_giving_up() {
    let h = Harness::new().await;
    let fakes = h.scratch_root().with_file_name("fakes");
    let argv = fakes.join("argv");
    let runner = DockerRunner {
        program: fake_docker(&fakes, &argv, false),
        image: "img:1".into(),
    };
    let paths = h.job_paths();
    let mut log = EventLog::open_append(paths.events()).unwrap();
    let unwritable = fakes.join("no-such-directory").join("job.log");

    let running = runner
        .launch(
            &h.launch_spec_for("x").await,
            &token(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let collected = collect(running, &mut log, &unwritable, CancellationToken::new()).await;

    assert!(collected.is_err());
    let calls = std::fs::read_to_string(&argv).unwrap();
    assert!(
        calls.lines().any(|l| l.starts_with("stop -t 30 al-1-1-")),
        "the container was never stopped: {calls}"
    );
    assert!(
        calls.lines().any(|l| l.starts_with("rm -f al-1-1-")),
        "the container was left running: {calls}"
    );
}

/// Cancelling stops the container gracefully, so `run` stops its agent and
/// reports the round itself — as it does under the other runners — rather
/// than being killed with its verdict unsaid.
#[tokio::test]
async fn cancelling_stops_the_container_so_run_reports_the_round() {
    let h = Harness::with_config(&support::config_running("sleeping-agent.sh")).await;
    let fakes = h.scratch_root().with_file_name("fakes");
    let argv = fakes.join("argv");
    let runner = DockerRunner {
        program: fake_docker(&fakes, &argv, false),
        image: "img:1".into(),
    };
    let paths = h.job_paths();
    let mut log = EventLog::open_append(paths.events()).unwrap();
    let cancel = CancellationToken::new();
    let later = cancel.clone();
    let output = paths.log();
    tokio::spawn(async move {
        // Once the agent runs, so the cancel reaches a round in progress.
        while !std::fs::read_to_string(&output).is_ok_and(|log| log.contains("sleeping-agent:")) {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        later.cancel();
    });

    let running = runner
        .launch(
            &h.launch_spec_for("x").await,
            &token(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let verdict = collect(running, &mut log, &paths.log(), cancel)
        .await
        .unwrap();

    assert!(!verdict.passed());
    let events = EventLog::read(paths.events()).unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(&e.kind, EventKind::RoundFailed { reason } if reason == "cancelled")),
        "{events:?}"
    );
    let calls = std::fs::read_to_string(&argv).unwrap();
    assert!(
        calls.lines().any(|l| l.starts_with("stop -t 30 al-1-1-")),
        "{calls}"
    );
    assert!(
        calls.lines().any(|l| l.starts_with("rm -f al-1-1-")),
        "the container was left behind: {calls}"
    );
}

/// A `docker` whose container prints far more than a pipe holds once told to
/// stop, and whose `stop`, like the real one, returns only after the
/// container has exited.
fn fake_docker_flooding_as_it_stops(dir: &Path, argv_log: &Path) -> PathBuf {
    fake_cli(
        dir,
        "docker",
        &format!(
            "echo \"$*\" >> {log}\n\
             case \"$1\" in\n\
               run) echo $$ > {pid}\n\
                    trap 'yes winding-down | head -n 200000; exit 143' TERM\n\
                    echo started\n\
                    while :; do sleep 0.05; done ;;\n\
               stop) kill -TERM \"$(cat {pid})\"\n\
                     while kill -0 \"$(cat {pid})\" 2>/dev/null; do sleep 0.05; done ;;\n\
               *) ;;\n\
             esac\n",
            log = argv_log.display(),
            pid = dir.join("container.pid").display(),
        ),
    )
}

/// Were the stream left unread while `docker stop` ran, the container would
/// block writing and `docker stop` would wait on it for good.
#[tokio::test]
async fn cancelling_a_container_that_prints_as_it_stops_still_ends_the_round() {
    let h = Harness::new().await;
    let fakes = h.scratch_root().with_file_name("fakes");
    let argv = fakes.join("argv");
    let runner = DockerRunner {
        program: fake_docker_flooding_as_it_stops(&fakes, &argv),
        image: "img:1".into(),
    };
    let paths = h.job_paths();
    let mut log = EventLog::open_append(paths.events()).unwrap();
    let cancel = CancellationToken::new();
    let later = cancel.clone();
    let output = paths.log();
    tokio::spawn(async move {
        while !std::fs::read_to_string(&output).is_ok_and(|log| log.contains("started")) {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        later.cancel();
    });

    let running = runner
        .launch(
            &h.launch_spec_for("x").await,
            &token(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let collected = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        collect(running, &mut log, &paths.log(), cancel),
    )
    .await;

    assert!(collected.is_ok(), "the round never ended after cancel");
    let calls = std::fs::read_to_string(&argv).unwrap();
    assert!(
        calls.lines().any(|l| l.starts_with("rm -f al-1-1-")),
        "the container was left behind: {calls}"
    );
}

#[tokio::test]
async fn an_oom_killed_container_is_reported_by_its_reason() {
    let h = Harness::with_config(&support::config_running("failing-agent.sh")).await;
    let fakes = h.scratch_root().with_file_name("fakes");
    let runner = DockerRunner {
        program: fake_docker(&fakes, &fakes.join("argv"), true),
        image: "img:1".into(),
    };
    let mut running = runner
        .launch(
            &h.launch_spec_for("x").await,
            &token(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();

    while running.next_line().await.is_some() {}
    let termination = running.termination().await;

    assert_eq!(termination.to_string(), "out of memory");
}

#[tokio::test]
async fn an_unreachable_docker_is_a_preflight_problem() {
    let tmp = tempfile::tempdir().unwrap();
    let runner = DockerRunner {
        program: fake_cli(
            tmp.path(),
            "docker",
            "echo 'Cannot connect to the Docker daemon' >&2\nexit 1\n",
        ),
        image: "img:1".into(),
    };

    let problems = runner.reasons_it_cannot_run().await;
    assert!(
        matches!(
            problems.as_slice(),
            [RunnerProblem::Unreachable {
                runner: "docker",
                ..
            }]
        ),
        "{problems:?}"
    );
}
