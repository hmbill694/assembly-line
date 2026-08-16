use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

/// Why a command stopped. Every variant except `Exited(0)` fails its node.
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

    /// A human-readable reason, or `None` when the command succeeded.
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

/// Run `cmd` under `sh -c`, appending both stdout and stderr to `log_path`.
///
/// The child gets two independent append-mode descriptors on the same file, so
/// output goes straight through the kernel and survives a kill — nothing is
/// buffered in this process and there are no reader tasks to drain.
///
/// # Errors
///
/// Returns an error if the log file cannot be opened or `sh` cannot be
/// spawned. A command that runs and fails is *not* an error — that is a
/// `ShellOutcome`, because a failing node is a normal part of a run.
pub async fn run_shell(
    cmd: &str,
    cwd: impl AsRef<Path>,
    log_path: impl AsRef<Path>,
    timeout: Option<Duration>,
    cancel: CancellationToken,
) -> anyhow::Result<ShellOutcome> {
    let log_path = log_path.as_ref();

    let mut child = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(cwd.as_ref())
        .stdin(Stdio::null())
        .stdout(Stdio::from(open_log_for_append(log_path)?))
        .stderr(Stdio::from(open_log_for_append(log_path)?))
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| anyhow::anyhow!("spawning `{cmd}`: {e}"))?;

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
