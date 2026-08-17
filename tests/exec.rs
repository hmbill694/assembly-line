use assembly_line::exec::{ShellOutcome, run_command, run_shell};
use assembly_line::provider::CommandSpec;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

fn spec(program: &str, args: &[&str]) -> CommandSpec {
    CommandSpec {
        program: program.to_string(),
        args: args.iter().map(|a| (*a).to_string()).collect(),
    }
}

#[tokio::test]
async fn captures_stdout_and_stderr_and_reports_exit_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("node.log");

    let out = run_shell(
        "echo hello; echo oops 1>&2",
        tmp.path(),
        &log,
        None,
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(out, ShellOutcome::Exited(0));
    assert!(out.succeeded());
    assert!(out.failure_reason().is_none());

    let body = std::fs::read_to_string(&log).unwrap();
    assert!(body.contains("hello"), "{body}");
    assert!(body.contains("oops"), "{body}");
}

#[tokio::test]
async fn reports_a_nonzero_exit_code() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run_shell(
        "exit 3",
        tmp.path(),
        tmp.path().join("l.log"),
        None,
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(out, ShellOutcome::Exited(3));
    assert!(!out.succeeded());
    assert_eq!(out.failure_reason().as_deref(), Some("exit 3"));
}

#[tokio::test]
async fn runs_in_the_given_working_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("l.log");
    run_shell("pwd", tmp.path(), &log, None, CancellationToken::new())
        .await
        .unwrap();

    // macOS reports /private/var for /var, so compare on the final component.
    let want = tmp.path().file_name().unwrap().to_str().unwrap();
    let body = std::fs::read_to_string(&log).unwrap();
    assert!(body.contains(want), "{body}");
}

#[tokio::test]
async fn kills_the_child_when_the_timeout_expires() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run_shell(
        "sleep 30",
        tmp.path(),
        tmp.path().join("l.log"),
        Some(Duration::from_millis(150)),
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(out, ShellOutcome::TimedOut);
    assert_eq!(out.failure_reason().as_deref(), Some("timed out"));
}

#[tokio::test]
async fn kills_the_child_when_cancelled() {
    let tmp = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        trigger.cancel();
    });

    let out = run_shell(
        "sleep 30",
        tmp.path(),
        tmp.path().join("l.log"),
        None,
        cancel,
    )
    .await
    .unwrap();

    assert_eq!(out, ShellOutcome::Cancelled);
}

#[tokio::test]
async fn an_already_cancelled_token_stops_the_command_immediately() {
    let tmp = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();
    cancel.cancel();

    let out = run_shell(
        "sleep 30",
        tmp.path(),
        tmp.path().join("l.log"),
        None,
        cancel,
    )
    .await
    .unwrap();

    assert_eq!(out, ShellOutcome::Cancelled);
}

#[tokio::test]
async fn appends_rather_than_truncating_across_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("l.log");

    for cmd in ["echo first", "echo second"] {
        run_shell(cmd, tmp.path(), &log, None, CancellationToken::new())
            .await
            .unwrap();
    }

    let body = std::fs::read_to_string(&log).unwrap();
    assert!(body.contains("first") && body.contains("second"), "{body}");
}

#[tokio::test]
async fn run_command_captures_output_and_exit_code() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("node.log");

    let outcome = run_command(
        &spec("sh", &["-c", "echo hello; echo oops 1>&2; exit 2"]),
        tmp.path(),
        &log,
        None,
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(outcome, ShellOutcome::Exited(2));
    let body = std::fs::read_to_string(&log).unwrap();
    assert!(body.contains("hello") && body.contains("oops"), "{body}");
}

#[tokio::test]
async fn run_command_passes_arguments_without_shell_interpretation() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("node.log");

    // If this went through a shell, the backticks and `$HOME` would expand
    // and the semicolon would split the command.
    let literal = "a `b` ; c $HOME";
    run_command(
        &spec("printf", &["%s", literal]),
        tmp.path(),
        &log,
        None,
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(std::fs::read_to_string(&log).unwrap(), literal);
}

#[tokio::test]
async fn run_command_honours_the_timeout() {
    let tmp = tempfile::tempdir().unwrap();
    let outcome = run_command(
        &spec("sleep", &["30"]),
        tmp.path(),
        tmp.path().join("l.log"),
        Some(Duration::from_millis(150)),
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(outcome, ShellOutcome::TimedOut);
}

#[tokio::test]
async fn run_command_reports_a_missing_program_as_an_error_not_an_exit_code() {
    let tmp = tempfile::tempdir().unwrap();
    let err = run_command(
        &spec("definitely-not-a-real-program-xyz", &[]),
        tmp.path(),
        tmp.path().join("l.log"),
        None,
        CancellationToken::new(),
    )
    .await
    .unwrap_err()
    .to_string();

    assert!(
        err.contains("definitely-not-a-real-program-xyz"),
        "the error must name the program so a bad provider config is obvious: {err}"
    );
}
