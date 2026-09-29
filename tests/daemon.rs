//! `assembly daemon`: one per root, checked before it listens, reachable on
//! its socket.

use assembly_line::daemon::api::Queued;
use assembly_line::daemon::client::{ClientError, DaemonClient};
use assert_cmd::Command;
use nix::sys::signal::Signal;
use predicates::str::contains;
use std::io::Write;
use std::os::unix::net::UnixStream;
use support::daemon::RunningDaemon;

mod support;

#[tokio::test]
async fn a_daemon_answers_on_its_socket_until_stopped() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = RunningDaemon::start(tmp.path(), &[], &[]);

    let health = DaemonClient::for_root(tmp.path()).health().await.unwrap();
    assert_eq!(health.version, env!("CARGO_PKG_VERSION"));

    assert!(daemon.stop().success());
    assert!(
        !tmp.path().join("daemon.sock").exists(),
        "the socket outlived the daemon"
    );
}

#[test]
fn ctrl_c_stops_a_daemon_as_sigterm_does() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = RunningDaemon::start(tmp.path(), &[], &[]);

    assert!(daemon.stop_with(Signal::SIGINT).success());
    assert!(!tmp.path().join("daemon.sock").exists());
}

#[test]
fn a_client_stalled_mid_request_does_not_keep_the_daemon_running() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = RunningDaemon::start(tmp.path(), &[], &[]);
    let mut stalled = UnixStream::connect(tmp.path().join("daemon.sock")).unwrap();
    stalled
        .write_all(b"GET /health HTTP/1.1\r\nHost: x\r\n")
        .unwrap();
    // Until the daemon has read those bytes the connection is idle, and an
    // idle connection never held shutdown up.
    std::thread::sleep(std::time::Duration::from_millis(300));

    assert!(daemon.stop().success());
    assert!(!tmp.path().join("daemon.sock").exists());
}

#[test]
fn a_root_that_is_a_file_is_refused_by_name() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("not-a-directory");
    std::fs::write(&file, "").unwrap();

    Command::cargo_bin("assembly")
        .unwrap()
        .args(["daemon", "--root"])
        .arg(&file)
        .assert()
        .code(2)
        .stderr(contains(file.display().to_string()))
        .stderr(contains("--root"));
}

#[tokio::test]
async fn with_no_daemon_the_client_says_how_to_start_one() {
    let tmp = tempfile::tempdir().unwrap();

    let err = DaemonClient::for_root(tmp.path())
        .health()
        .await
        .unwrap_err();

    assert!(matches!(err, ClientError::NoDaemon { .. }), "{err}");
    assert!(err.to_string().contains("assembly daemon"), "{err}");
}

/// A client and daemon from different versions can disagree about a
/// submission's shape; the daemon's own words for that must reach the user.
#[tokio::test]
async fn a_submission_the_daemon_cannot_read_is_reported_in_its_words() {
    let tmp = tempfile::tempdir().unwrap();
    let _daemon = RunningDaemon::start(tmp.path(), &[], &[]);

    let err = DaemonClient::for_root(tmp.path())
        .post_json::<_, Queued>("/jobs", &serde_json::json!({ "remote_url": "x" }))
        .await
        .unwrap_err();

    assert!(err.to_string().contains("missing field"), "{err}");
}

#[test]
fn a_second_daemon_on_one_root_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let _first = RunningDaemon::start(tmp.path(), &[], &[]);

    Command::cargo_bin("assembly")
        .unwrap()
        .args(["daemon", "--root"])
        .arg(tmp.path())
        .assert()
        .code(2)
        .stderr(contains("another daemon holds"));
}

/// Review focus 3: a daemon that could not clean up leaves its socket and
/// lock file behind, and neither may stop the next one.
#[tokio::test]
async fn a_daemon_killed_outright_leaves_a_root_the_next_one_can_take() {
    let tmp = tempfile::tempdir().unwrap();
    RunningDaemon::start(tmp.path(), &[], &[]).kill();
    assert!(tmp.path().join("daemon.sock").exists());
    assert!(matches!(
        DaemonClient::for_root(tmp.path()).health().await,
        Err(ClientError::NoDaemon { .. })
    ));

    let _next = RunningDaemon::start(tmp.path(), &[], &[]);

    assert!(DaemonClient::for_root(tmp.path()).health().await.is_ok());
}

/// Review focus 5.
#[test]
fn a_root_too_deep_for_a_socket_is_refused_by_name() {
    let tmp = tempfile::tempdir().unwrap();
    let deep = tmp.path().join("d".repeat(120));

    Command::cargo_bin("assembly")
        .unwrap()
        .args(["daemon", "--root"])
        .arg(&deep)
        .assert()
        .code(2)
        .stderr(contains("--root"));
    assert!(!deep.exists(), "a refused root was created anyway");
}

#[test]
fn a_daemon_whose_runner_cannot_run_does_not_start() {
    let tmp = tempfile::tempdir().unwrap();
    let fakes = tmp.path().join("fakes");
    support::fake_cli(&fakes, "docker", "echo 'no daemon' >&2\nexit 1\n");

    Command::cargo_bin("assembly")
        .unwrap()
        .args(["daemon", "--runner", "docker", "--root"])
        .arg(tmp.path().join("root"))
        .env(
            "PATH",
            format!("{}:{}", fakes.display(), std::env::var("PATH").unwrap()),
        )
        .env_remove("ASSEMBLY_GIT_TOKEN")
        .env_remove("GH_TOKEN")
        .assert()
        .code(2)
        .stderr(contains("`docker` cannot be reached"))
        .stderr(contains("$ASSEMBLY_GIT_TOKEN is not set"))
        .stderr(contains("$GH_TOKEN is not set"));
    assert!(!tmp.path().join("root/daemon.sock").exists());
}

#[test]
fn a_daemon_allowed_no_jobs_at_all_is_a_usage_error() {
    let tmp = tempfile::tempdir().unwrap();

    Command::cargo_bin("assembly")
        .unwrap()
        .args(["daemon", "--max-jobs=0", "--root"])
        .arg(tmp.path())
        .assert()
        .code(2)
        .stderr(contains("--max-jobs"));
    assert!(!tmp.path().join("daemon.lock").exists());
}
