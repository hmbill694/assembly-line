//! Running a round as a process on this machine: no isolation, the host's
//! own toolchain and credentials.

use super::tail::FileTail;
use super::{
    JobSecrets, LaunchSpec, RoundHandle, Runner, RunnerProblem, RunningRound, Termination,
};
use crate::payload::GIT_TOKEN_VAR;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::{Child, Command};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct LocalRunner {
    program: PathBuf,
}

impl LocalRunner {
    /// This very binary, which is what `assembly run` is.
    ///
    /// # Errors
    ///
    /// Returns an error if the OS cannot say where this binary is.
    pub fn current_binary() -> std::io::Result<Self> {
        std::env::current_exe().map(Self::using)
    }

    /// A specific `assembly` binary — tests name the one cargo built.
    pub fn using(program: impl Into<PathBuf>) -> Self {
        LocalRunner {
            program: program.into(),
        }
    }
}

impl Runner for LocalRunner {
    type Running = LocalRound;
    const RUNS_IN_A_CONTAINER: bool = false;

    /// The host is already here: nothing to reach, nothing to create.
    fn reasons_it_cannot_run(&self) -> impl Future<Output = Vec<RunnerProblem>> + Send {
        std::future::ready(Vec::new())
    }

    /// `secrets` goes unused: the process inherits the host's environment,
    /// git credentials included. Spawning does not wait on it, so the launch
    /// is ready at once, with nothing for `cancel` to interrupt.
    fn launch(
        &self,
        spec: &LaunchSpec,
        _secrets: &JobSecrets,
        _cancel: &CancellationToken,
    ) -> impl Future<Output = anyhow::Result<LocalRound>> + Send {
        std::future::ready(self.spawn_run(spec))
    }

    /// A round `run` is still writing, or has finished writing, to its
    /// frames file. Its exit status is not this process's to learn: `run`
    /// was an earlier daemon's child.
    fn reattach(
        &self,
        handle: &RoundHandle,
    ) -> impl Future<Output = anyhow::Result<LocalRound>> + Send {
        std::future::ready(match handle {
            RoundHandle::Local { pid, frames } => FileTail::open(frames)
                .map(|tail| LocalRound {
                    pid: *pid,
                    frames: frames.clone(),
                    tail,
                    ours: None,
                })
                .map_err(|e| anyhow::anyhow!("reading {}: {e}", frames.display())),
            other => Err(other.launched_by_another_runner("local")),
        })
    }
}

impl LocalRunner {
    /// `run` leading a session of its own, with stdout and stderr on the
    /// spec's frames file — not a pipe, so it outlives this process.
    fn spawn_run(&self, spec: &LaunchSpec) -> anyhow::Result<LocalRound> {
        if let Some(dir) = spec.frames_file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let frames = std::fs::File::create(&spec.frames_file)
            .map_err(|e| anyhow::anyhow!("opening {}: {e}", spec.frames_file.display()))?;
        let tail = FileTail::open(&spec.frames_file)?;
        let mut command = Command::new(&self.program);
        crate::exec::detach_from_terminal(&mut command)
            .args(&spec.args)
            // Exported for a container runner, the token would make `run`
            // authenticate with it instead of the host's own credentials.
            .env_remove(GIT_TOKEN_VAR)
            .stdin(Stdio::null())
            .stdout(frames.try_clone()?)
            .stderr(frames);
        let child = command
            .spawn()
            .map_err(|e| anyhow::anyhow!("spawning `{}`: {e}", self.program.display()))?;
        let pid = child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .ok_or_else(|| anyhow::anyhow!("`run` exited before it could be watched"))?;
        Ok(LocalRound {
            pid,
            frames: spec.frames_file.clone(),
            tail,
            ours: Some(child),
        })
    }
}

/// `run` leading its own session, read back from its frames file. `ours` is
/// the child when this process launched it; a reattached round was launched
/// by an earlier daemon and is known only by its pid.
#[derive(Debug)]
pub struct LocalRound {
    pid: i32,
    frames: PathBuf,
    tail: FileTail,
    ours: Option<Child>,
}

/// Whether `run` is still running. A child of ours is asked directly, which
/// also reaps it. Anyone else's pid counts only while it leads a session of
/// its own, as `run` does: a pid the system has handed to an ordinary process
/// since is not taken for the round. One handed to another session leader
/// still is, until that process exits.
fn still_running(pid: i32, ours: &mut Option<Child>) -> bool {
    match ours {
        Some(child) => matches!(child.try_wait(), Ok(None)),
        None => leads_its_own_session(pid),
    }
}

fn leads_its_own_session(pid: i32) -> bool {
    let pid = Pid::from_raw(pid);
    nix::unistd::getsid(Some(pid)) == Ok(pid)
}

impl RunningRound for LocalRound {
    async fn next_line(&mut self) -> Option<String> {
        let LocalRound {
            pid, tail, ours, ..
        } = self;
        tail.next_line(|| still_running(*pid, ours)).await
    }

    /// SIGTERM to the round's session, which `run` leads: `run` answers by
    /// cancelling its agent and reporting the round. Returns at once; the
    /// round's end arrives on the stream.
    fn cancel(&mut self) -> impl Future<Output = ()> + Send {
        // A pid already reaped, or no longer a session leader, may belong to
        // someone else by now.
        let still_ours = match &self.ours {
            Some(child) => child.id().is_some(),
            None => leads_its_own_session(self.pid),
        };
        if still_ours {
            let _ = killpg(Pid::from_raw(self.pid), Signal::SIGTERM);
        }
        std::future::ready(())
    }

    async fn termination(self) -> Termination {
        match self.ours {
            Some(mut child) => Termination::Exited(
                child
                    .wait()
                    .await
                    .ok()
                    .and_then(|status| status.code())
                    .unwrap_or(-1),
            ),
            None => Termination::Killed {
                reason: "it ran while the daemon was down, so its exit status is unknown".into(),
            },
        }
    }

    fn handle(&self) -> RoundHandle {
        RoundHandle::Local {
            pid: self.pid,
            frames: self.frames.clone(),
        }
    }
}
