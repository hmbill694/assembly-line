use assembly_line::collect::collect;
use assembly_line::event::{EventKind, EventLog};
use assembly_line::payload::{FORGE_TOKEN_VAR, GIT_TOKEN_VAR};
use assembly_line::runner::docker::DockerRunner;
use assembly_line::runner::{JobSecrets, Runner, RunnerProblem, RunningRound};
use std::path::{Path, PathBuf};
use support::{Harness, fake_cli};
use tokio_util::sync::CancellationToken;

mod support;

/// A `docker` that records its argv and runs `assembly` as the container.
fn fake_docker(dir: &Path, argv_log: &Path, oom: bool) -> PathBuf {
    fake_docker_running(
        dir,
        argv_log,
        Path::new(env!("CARGO_BIN_EXE_assembly")),
        oom,
    )
}

/// A `docker` that records its argv and keeps each container's state in
/// files under `dir`. `create` keeps the arguments after `assembly`, and
/// the values of the variables `-e` names, as `docker create -e NAME` takes
/// them from the client's environment; `start` runs `entrypoint` with them
/// in the background, its output to the container's log; `logs -f` follows
/// that log until the container exits; `wait` prints its exit code; and
/// `stop` sends it SIGTERM and returns once it has exited, as the real one
/// does.
fn fake_docker_running(dir: &Path, argv_log: &Path, entrypoint: &Path, oom: bool) -> PathBuf {
    fake_cli(
        dir,
        "docker",
        &format!(
            r#"echo "$*" >> {log}
state={state}
mkdir -p "$state"
case "$1" in
  create)
    shift; name=""
    while [ "$1" != assembly ]; do
      case "$1" in
        --name) name=$2; shift ;;
        -e) printf 'export %s=%q\n' "$2" "${{!2-}}" >> "$state/env.tmp"; shift ;;
      esac
      shift
    done
    shift
    mv "$state/env.tmp" "$state/$name.env" 2>/dev/null || : > "$state/$name.env"
    printf '%q ' "$@" > "$state/$name.args"
    echo "$name" ;;
  start)
    name=$2
    : > "$state/$name.log"
    ( . "$state/$name.env"
      eval "set -- $(cat "$state/$name.args")"
      status=0
      {entrypoint} "$@" > "$state/$name.log" 2>&1 || status=$?
      echo "$status" > "$state/$name.exit" ) > /dev/null 2>&1 < /dev/null &
    echo $! > "$state/$name.pid" ;;
  logs)
    name=$3; shown=0
    while :; do
      ended=no
      if [ -e "$state/$name.exit" ]; then ended=yes; fi
      total=$(wc -l < "$state/$name.log" | tr -d ' ')
      if [ "$total" -gt "$shown" ]; then
        sed -n "$((shown + 1)),${{total}}p" "$state/$name.log"
        shown=$total
      fi
      if [ "$ended" = yes ]; then break; fi
      sleep 0.05
    done ;;
  wait)
    name=$2
    while [ ! -e "$state/$name.exit" ]; do sleep 0.05; done
    cat "$state/$name.exit" ;;
  stop)
    name=$4
    pkill -TERM -P "$(cat "$state/$name.pid")" || true
    while [ ! -e "$state/$name.exit" ]; do sleep 0.05; done ;;
  version) echo 27.0.0 ;;
  inspect) echo {oom} ;;
  *) ;;
esac
"#,
            log = argv_log.display(),
            state = dir.join("containers").display(),
            entrypoint = entrypoint.display(),
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
    let verdict = collect(
        running,
        &mut log,
        &paths.log(),
        &paths.position(1),
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert!(
        verdict.passed(),
        "{:?}",
        EventLog::read(paths.events()).unwrap()
    );
    let calls = std::fs::read_to_string(&argv).unwrap();
    assert!(calls.contains("create --name al-1-1-"), "{calls}");
    assert!(
        calls.lines().any(|l| l.starts_with("start al-1-1-")),
        "{calls}"
    );
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
    collect(
        running,
        &mut log,
        &paths.log(),
        &paths.position(1),
        CancellationToken::new(),
    )
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
    let collected = collect(
        running,
        &mut log,
        &unwritable,
        &paths.position(1),
        CancellationToken::new(),
    )
    .await;

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
    let verdict = collect(running, &mut log, &paths.log(), &paths.position(1), cancel)
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
/// stop.
fn fake_docker_flooding_as_it_stops(dir: &Path, argv_log: &Path) -> PathBuf {
    let container = fake_cli(
        dir,
        "container",
        "trap 'yes winding-down | head -n 200000; exit 143' TERM\n\
         echo started\n\
         while :; do sleep 0.05; done\n",
    );
    fake_docker_running(dir, argv_log, &container, false)
}

/// A cancelled container that floods its log on the way out still ends its
/// round, and is removed.
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
        collect(running, &mut log, &paths.log(), &paths.position(1), cancel),
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

#[tokio::test]
async fn a_docker_round_is_reattached_by_its_container_name() {
    let h = Harness::new().await;
    let fakes = h.scratch_root().with_file_name("fakes");
    let argv = fakes.join("argv");
    let runner = DockerRunner {
        program: fake_docker(&fakes, &argv, false),
        image: "img:1".into(),
    };
    let first = runner
        .launch(
            &h.launch_spec_for("write a file").await,
            &token(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let handle = first.handle();
    drop(first);

    let again = runner.reattach(&handle).await.unwrap();
    let paths = h.job_paths();
    let mut log = EventLog::open_append(paths.events()).unwrap();
    let verdict = collect(
        again,
        &mut log,
        &paths.log(),
        &paths.position(1),
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert!(verdict.passed());
    let output = std::fs::read_to_string(paths.log()).unwrap();
    assert_eq!(
        output.matches("fake-agent: write a file").count(),
        1,
        "{output}"
    );
    let calls = std::fs::read_to_string(&argv).unwrap();
    assert_eq!(
        calls.lines().filter(|l| l.starts_with("create")).count(),
        1,
        "{calls}"
    );
}

/// Accepted risk 12, closed: a cancel while the image is still pulling
/// leaves no container behind to start later.
#[tokio::test]
async fn a_round_cancelled_while_its_container_is_being_created_leaves_none() {
    let h = Harness::new().await;
    let fakes = h.scratch_root().with_file_name("fakes");
    let argv = fakes.join("argv");
    let creating = fakes.join("creating");
    let runner = DockerRunner {
        program: fake_cli(
            &fakes,
            "docker",
            &format!(
                "echo \"$*\" >> {argv}\n\
                 if [ \"$1\" = create ]; then touch {creating}; exec sleep 30; fi\n",
                argv = argv.display(),
                creating = creating.display(),
            ),
        ),
        image: "img:1".into(),
    };
    let spec = h.launch_spec_for("x").await;
    let cancel = CancellationToken::new();
    let cancel_once_pulling = cancel.clone();
    tokio::spawn(async move {
        while !creating.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        cancel_once_pulling.cancel();
    });

    let launched = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        runner.launch(&spec, &token(), &cancel),
    )
    .await
    .expect("the cancel waited out the pull");

    let err = launched.unwrap_err().to_string();
    assert!(
        err.contains("cancelled before the container started"),
        "{err}"
    );
    let calls = std::fs::read_to_string(&argv).unwrap();
    assert!(
        calls.lines().any(|l| l == format!("rm -f {}", spec.name)),
        "{calls}"
    );
    assert!(!calls.lines().any(|l| l.starts_with("start")), "{calls}");
}
