use crate::frame::FrameWriter;
use crate::payload::{FORGE_TOKEN_VAR, GIT_TOKEN_VAR};
use crate::provider::CommandSpec;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use std::io::Write;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::task::{AbortHandle, JoinHandle};
use tokio_util::sync::CancellationToken;

/// How long to keep forwarding output after a command exits. A command that
/// backgrounded a grandchild holding its stdout open would otherwise hold
/// the round forever.
const OUTPUT_DRAIN_GRACE: Duration = Duration::from_secs(2);

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

/// Under `sh -c`, for `verify`: the user wrote a shell line and expects pipes
/// and redirection to work.
///
/// # Errors
///
/// Returns an error if `sh` cannot be spawned. A command that runs and fails
/// is *not* an error — that is a `ShellOutcome`, because a failing round is a
/// normal part of using this.
pub async fn run_shell<W: Write + Send + 'static>(
    cmd: &str,
    cwd: impl AsRef<Path>,
    output: &FrameWriter<W>,
    timeout: Option<Duration>,
    cancel: CancellationToken,
) -> anyhow::Result<ShellOutcome> {
    let mut command = Command::new("sh");
    command.arg("-c").arg(cmd);
    supervise(command, cmd, cwd, output, timeout, cancel).await
}

/// Bypassing the shell — see [`CommandSpec`] for why agent commands must.
///
/// # Errors
///
/// Returns an error if the program cannot be spawned — a missing binary
/// means the provider config is wrong, which is worth distinguishing from an
/// agent that ran and failed.
pub async fn run_command<W: Write + Send + 'static>(
    spec: &CommandSpec,
    cwd: impl AsRef<Path>,
    output: &FrameWriter<W>,
    timeout: Option<Duration>,
    cancel: CancellationToken,
) -> anyhow::Result<ShellOutcome> {
    let mut command = Command::new(&spec.program);
    command.args(&spec.args);
    supervise(command, &spec.program, cwd, output, timeout, cancel).await
}

/// Wait for whichever comes first: exit, deadline, or cancellation. Shared so
/// the timeout and kill semantics cannot drift between the two entry points.
///
/// The command leads its own process group (see [`detach_from_terminal`]),
/// and the group is killed however the command ends — timed out, cancelled,
/// or exited on its own. An agent's shell stopped alone would leave its
/// `sleep` or test runner behind, orphaned, still running in a checkout that
/// is about to be deleted.
async fn supervise<W: Write + Send + 'static>(
    mut command: Command,
    described_as: &str,
    cwd: impl AsRef<Path>,
    output: &FrameWriter<W>,
    timeout: Option<Duration>,
    cancel: CancellationToken,
) -> anyhow::Result<ShellOutcome> {
    let mut child = detach_from_terminal(&mut command)
        .current_dir(cwd.as_ref())
        // The git and forge tokens are the job's to push and open its pull
        // request with, so they are kept out of the agent's environment —
        // though not out of its reach; see `GIT_TOKEN_VAR`.
        .env_remove(GIT_TOKEN_VAR)
        .env_remove(FORGE_TOKEN_VAR)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| anyhow::anyhow!("spawning `{described_as}`: {e}"))?;

    // Taken now: once the command is reaped, `id()` no longer answers.
    let group = child
        .id()
        .and_then(|id| i32::try_from(id).ok())
        .map(Pid::from_raw);

    let forwarding = [
        forward_as_output(child.stdout.take(), output.clone()),
        forward_as_output(child.stderr.take(), output.clone()),
    ];
    let forwarders: Vec<AbortHandle> = forwarding.iter().map(JoinHandle::abort_handle).collect();

    let deadline = async {
        match timeout {
            Some(d) => tokio::time::sleep(d).await,
            None => std::future::pending::<()>().await,
        }
    };

    let ended = tokio::select! {
        status = child.wait() => status.map(|s| ShellOutcome::Exited(s.code().unwrap_or(-1))),
        () = deadline => {
            stop_with_its_group(&mut child, group).await;
            Ok(ShellOutcome::TimedOut)
        }
        () = cancel.cancelled() => {
            stop_with_its_group(&mut child, group).await;
            Ok(ShellOutcome::Cancelled)
        }
    };
    // A command that exited on its own may have left what it backgrounded
    // running in its group.
    kill_group(group);

    // Every line the command printed is framed before the round's next
    // event, so the log reads in the order things happened.
    let [stdout_forwarded, stderr_forwarded] = forwarding;
    let drained = async {
        let _ = tokio::join!(stdout_forwarded, stderr_forwarded);
    };
    if tokio::time::timeout(OUTPUT_DRAIN_GRACE, drained)
        .await
        .is_err()
    {
        // Something outside the group still holds the pipes. Whatever it
        // prints from here on would land after the round's later events.
        forwarders.iter().for_each(AbortHandle::abort);
    }
    Ok(ended?)
}

/// SIGKILL the command and everything in its process group, then reap it.
/// The group goes first, while its unreaped leader still holds the group id
/// — so the id cannot yet belong to anyone else. Only a command that exits
/// on its own is reaped before its group is killed.
async fn stop_with_its_group(child: &mut Child, group: Option<Pid>) {
    kill_group(group);
    let _ = child.kill().await;
}

/// SIGKILL every process left in the group. An empty group is `ESRCH`,
/// which is the answer hoped for, so the result is ignored.
fn kill_group(group: Option<Pid>) {
    if let Some(group) = group {
        let _ = killpg(group, Signal::SIGKILL);
    }
}

/// Start `command` as the leader of a session of its own, and so of a
/// process group of its own too, with no controlling terminal.
///
/// A process group alone is not enough when assembly-line runs in a
/// terminal: the command's group is then a background group of it, and
/// anything in it that reads `/dev/tty` — an agent asking permission, `ssh`
/// confirming a host key, git asking for a username — is stopped by
/// `SIGTTIN` and never resumed. Without a controlling terminal that read
/// fails at once instead, the way it would in a container.
pub(crate) fn detach_from_terminal(command: &mut Command) -> &mut Command {
    // SAFETY: the closure runs in the forked child before `exec`, where only
    // async-signal-safe calls are allowed; `setsid` is one, and the closure
    // does nothing else.
    unsafe {
        command.pre_exec(|| {
            nix::unistd::setsid()
                .map(|_| ())
                .map_err(std::io::Error::from)
        })
    }
}

/// Forward each line `source` produces as an `output` frame, until it closes.
fn forward_as_output<W: Write + Send + 'static>(
    source: Option<impl AsyncRead + Unpin + Send + 'static>,
    output: FrameWriter<W>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let Some(source) = source else { return };
        let mut segments = BufReader::new(source).split(b'\n');
        while let Ok(Some(segment)) = segments.next_segment().await {
            if output
                .append_output(&String::from_utf8_lossy(&segment))
                .is_err()
            {
                return;
            }
        }
    })
}
