//! Running a round as a child of this process: no isolation, the host's own
//! toolchain and credentials.

use super::child::ChildLines;
use super::{JobSecrets, LaunchSpec, Runner, RunnerProblem, RunningRound, Termination};
use crate::payload::GIT_TOKEN_VAR;
use std::path::PathBuf;
use tokio::process::Command;
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

    /// `secrets` goes unused: the child inherits the host's environment, git
    /// credentials included. Spawning does not wait on the child, so the
    /// launch is ready at once, with nothing for `cancel` to interrupt.
    fn launch(
        &self,
        spec: &LaunchSpec,
        _secrets: &JobSecrets,
        _cancel: &CancellationToken,
    ) -> impl Future<Output = anyhow::Result<LocalRound>> + Send {
        std::future::ready(self.spawn_run(spec))
    }
}

impl LocalRunner {
    fn spawn_run(&self, spec: &LaunchSpec) -> anyhow::Result<LocalRound> {
        let mut command = Command::new(&self.program);
        command
            .args(&spec.args)
            // Exported for a container runner, the token would make `run`
            // authenticate with it instead of the host's own credentials.
            .env_remove(GIT_TOKEN_VAR);
        ChildLines::spawn(command).map(|lines| LocalRound { lines })
    }
}

#[derive(Debug)]
pub struct LocalRound {
    lines: ChildLines,
}

impl RunningRound for LocalRound {
    async fn next_line(&mut self) -> Option<String> {
        self.lines.next_line().await
    }

    /// SIGTERM, which `run` answers by cancelling its agent and
    /// reporting the round. Returns at once; the round's end arrives on the
    /// stream.
    async fn cancel(&mut self) {
        self.lines.terminate();
    }

    async fn termination(self) -> Termination {
        Termination::Exited(self.lines.exit_code().await)
    }
}
