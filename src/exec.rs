use crate::provider::CommandSpec;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

/// Why a command stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellOutcome {
    Exited(i32),
    TimedOut,
    Cancelled,
}

impl ShellOutcome {
    #[must_use]
    pub fn succeeded(&self) -> bool {
        matches!(self, Self::Exited(0))
    }

    #[must_use]
    pub fn failure_reason(&self) -> Option<String> {
        match self {
            Self::Exited(0) => None,
            Self::Exited(code) => Some(format!("exit {code}")),
            Self::TimedOut => Some("timed out".to_string()),
            Self::Cancelled => Some("cancelled".to_string()),
        }
    }
}

fn open_log_for_append(path: &Path) -> std::io::Result<std::fs::File> {
    path.parent()
        .map(std::fs::create_dir_all)
        .transpose()
        .and_then(|_| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
        })
}

/// Under `sh -c`, for `verify`: the user wrote a shell line and expects pipes
/// and redirection to work.
///
/// # Errors
///
/// Returns an error if the log file cannot be opened or `sh` cannot be
/// spawned. A command that runs and fails is *not* an error — that is a
/// `ShellOutcome`, because a failing job is a normal part of using this.
pub async fn run_shell(
    cmd: &str,
    cwd: impl AsRef<Path>,
    log_path: impl AsRef<Path>,
    timeout: Option<Duration>,
    cancel: CancellationToken,
) -> anyhow::Result<ShellOutcome> {
    let mut command = Command::new("sh");
    command.arg("-c").arg(cmd);
    supervise(command, cmd, cwd, log_path, timeout, cancel).await
}

/// Bypassing the shell — see [`CommandSpec`] for why agent commands must.
///
/// # Errors
///
/// Returns an error if the log file cannot be opened or the program cannot be
/// spawned — a missing binary means the provider config is wrong, which is
/// worth distinguishing from an agent that ran and failed.
pub async fn run_command(
    spec: &CommandSpec,
    cwd: impl AsRef<Path>,
    log_path: impl AsRef<Path>,
    timeout: Option<Duration>,
    cancel: CancellationToken,
) -> anyhow::Result<ShellOutcome> {
    let mut command = Command::new(&spec.program);
    command.args(&spec.args);
    supervise(command, &spec.program, cwd, log_path, timeout, cancel).await
}

/// Wait for whichever comes first: exit, deadline, or cancellation. Shared so
/// the timeout and kill semantics cannot drift between the two entry points.
///
/// The child gets two independent append-mode descriptors on the same file:
/// nothing is buffered in *this* process and there are no reader tasks to
/// drain, so a kill loses only whatever the child had buffered itself.
async fn supervise(
    mut command: Command,
    described_as: &str,
    cwd: impl AsRef<Path>,
    log_path: impl AsRef<Path>,
    timeout: Option<Duration>,
    cancel: CancellationToken,
) -> anyhow::Result<ShellOutcome> {
    let log_path = log_path.as_ref();

    let mut child = command
        .current_dir(cwd.as_ref())
        .stdin(Stdio::null())
        .stdout(Stdio::from(open_log_for_append(log_path)?))
        .stderr(Stdio::from(open_log_for_append(log_path)?))
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| anyhow::anyhow!("spawning `{described_as}`: {e}"))?;

    let deadline = async {
        match timeout {
            Some(d) => tokio::time::sleep(d).await,
            None => std::future::pending::<()>().await,
        }
    };

    tokio::select! {
        status = child.wait() => Ok(ShellOutcome::Exited(status?.code().unwrap_or(-1))),
        () = deadline => {
            let _ = child.kill().await;
            Ok(ShellOutcome::TimedOut)
        }
        () = cancel.cancelled() => {
            let _ = child.kill().await;
            Ok(ShellOutcome::Cancelled)
        }
    }
}
