use assembly_line::exec::{ShellOutcome, run_command, run_shell};
use assembly_line::frame::{FrameWriter, Routed, StreamPosition};
use assembly_line::provider::CommandSpec;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

fn spec(program: &str, args: &[&str]) -> CommandSpec {
    CommandSpec {
        program: program.to_string(),
        args: args.iter().map(|a| (*a).to_string()).collect(),
    }
}

fn printed(output: &FrameWriter<Vec<u8>>) -> Vec<String> {
    String::from_utf8(output.copy_of_sink())
        .unwrap()
        .lines()
        .filter_map(|line| match StreamPosition::default().route(line).1 {
            Routed::Output(text) => Some(text),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn captures_stdout_and_stderr_and_reports_exit_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let output = FrameWriter::new(Vec::new());

    let out = run_shell(
        "echo hello; echo oops 1>&2",
        tmp.path(),
        &output,
        None,
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(out, ShellOutcome::Exited(0));
    assert!(out.succeeded());
    assert!(out.failure_reason().is_none());

    let lines = printed(&output);
    assert!(lines.contains(&"hello".to_string()), "{lines:?}");
    assert!(lines.contains(&"oops".to_string()), "{lines:?}");
}

#[tokio::test]
async fn reports_a_nonzero_exit_code() {
    let tmp = tempfile::tempdir().unwrap();
    let output = FrameWriter::new(Vec::new());
    let out = run_shell(
        "exit 3",
        tmp.path(),
        &output,
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
    let output = FrameWriter::new(Vec::new());
    run_shell("pwd", tmp.path(), &output, None, CancellationToken::new())
        .await
        .unwrap();

    // macOS reports /private/var for /var, so compare on the final component.
    let want = tmp.path().file_name().unwrap().to_str().unwrap();
    let lines = printed(&output);
    assert!(lines.iter().any(|line| line.contains(want)), "{lines:?}");
}

#[tokio::test]
async fn kills_the_child_when_the_timeout_expires() {
    let tmp = tempfile::tempdir().unwrap();
    let output = FrameWriter::new(Vec::new());
    let out = run_shell(
        "sleep 30",
        tmp.path(),
        &output,
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
    let output = FrameWriter::new(Vec::new());
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        trigger.cancel();
    });

    let out = run_shell("sleep 30", tmp.path(), &output, None, cancel)
        .await
        .unwrap();

    assert_eq!(out, ShellOutcome::Cancelled);
}

#[tokio::test]
async fn an_already_cancelled_token_stops_the_command_immediately() {
    let tmp = tempfile::tempdir().unwrap();
    let output = FrameWriter::new(Vec::new());
    let cancel = CancellationToken::new();
    cancel.cancel();

    let out = run_shell("sleep 30", tmp.path(), &output, None, cancel)
        .await
        .unwrap();

    assert_eq!(out, ShellOutcome::Cancelled);
}

/// Whether a process with this pid still exists — `kill -0` delivers no
/// signal, it only asks.
fn process_is_alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

#[tokio::test]
async fn stopping_a_command_stops_what_it_started_too() {
    let tmp = tempfile::tempdir().unwrap();
    let output = FrameWriter::new(Vec::new());

    let out = run_shell(
        "sleep 30 & echo $!; wait",
        tmp.path(),
        &output,
        Some(Duration::from_millis(300)),
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(out, ShellOutcome::TimedOut);
    let grandchild = printed(&output).remove(0);
    assert!(
        vanishes(&grandchild),
        "the backgrounded `sleep` ({grandchild}) outlived its command"
    );
}

#[tokio::test]
async fn a_command_that_exits_on_its_own_leaves_nothing_running() {
    let tmp = tempfile::tempdir().unwrap();
    let output = FrameWriter::new(Vec::new());

    let out = run_shell(
        "sleep 30 & echo $!",
        tmp.path(),
        &output,
        None,
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(out, ShellOutcome::Exited(0));
    let grandchild = printed(&output).remove(0);
    assert!(
        vanishes(&grandchild),
        "the backgrounded `sleep` ({grandchild}) outlived its command"
    );
}

/// Whether the process is gone within two seconds. An orphan is reaped by
/// init, not by us, so it gets a moment to vanish.
fn vanishes(pid: &str) -> bool {
    (0..20).any(|_| {
        std::thread::sleep(Duration::from_millis(100));
        !process_is_alive(pid)
    })
}

#[tokio::test]
async fn run_command_captures_output_and_exit_code() {
    let tmp = tempfile::tempdir().unwrap();
    let output = FrameWriter::new(Vec::new());

    let outcome = run_command(
        &spec("sh", &["-c", "echo hello; echo oops 1>&2; exit 2"]),
        tmp.path(),
        &output,
        None,
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(outcome, ShellOutcome::Exited(2));
    let lines = printed(&output);
    assert!(
        lines.contains(&"hello".to_string()) && lines.contains(&"oops".to_string()),
        "{lines:?}"
    );
}

#[tokio::test]
async fn run_command_passes_arguments_without_shell_interpretation() {
    let tmp = tempfile::tempdir().unwrap();
    let output = FrameWriter::new(Vec::new());

    // If this went through a shell, the backticks and `$HOME` would expand
    // and the semicolon would split the command.
    let literal = "a `b` ; c $HOME";
    run_command(
        &spec("printf", &["%s", literal]),
        tmp.path(),
        &output,
        None,
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(printed(&output), [literal]);
}

#[tokio::test]
async fn run_command_honours_the_timeout() {
    let tmp = tempfile::tempdir().unwrap();
    let output = FrameWriter::new(Vec::new());
    let outcome = run_command(
        &spec("sleep", &["30"]),
        tmp.path(),
        &output,
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
    let output = FrameWriter::new(Vec::new());
    let err = run_command(
        &spec("definitely-not-a-real-program-xyz", &[]),
        tmp.path(),
        &output,
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

/// Run from a terminal, a command in a background process group that reads
/// the terminal is stopped and never resumed. With no terminal to open, the
/// read fails and the command carries on. (Run without a terminal, as in CI,
/// the command has none either way; this guards the case where there is one.)
#[tokio::test]
async fn a_command_has_no_terminal_to_wait_on() {
    let tmp = tempfile::tempdir().unwrap();
    let output = FrameWriter::new(Vec::new());

    let outcome = run_shell(
        "if (exec 3</dev/tty) 2>/dev/null; then echo 'has a terminal'; else echo 'no terminal'; fi",
        tmp.path(),
        &output,
        Some(Duration::from_secs(10)),
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(outcome, ShellOutcome::Exited(0));
    assert_eq!(printed(&output), ["no terminal"]);
}
