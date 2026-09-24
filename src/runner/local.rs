//! Running a job as a child of this process: no isolation, the host's own
//! toolchain and credentials.

use super::Termination;
use super::child::ChildLines;
use crate::payload::{JobPayload, PAYLOAD_VAR};
use std::path::PathBuf;
use tokio::process::Command;

#[derive(Debug, Clone)]
pub struct LocalRunner {
    program: PathBuf,
}

impl LocalRunner {
    /// This very binary, which is what `job-exec` is.
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

    /// # Errors
    ///
    /// Returns an error if the payload cannot be serialised or the binary
    /// cannot be spawned.
    pub fn launch(&self, payload: &JobPayload) -> anyhow::Result<LocalJob> {
        let mut command = Command::new(&self.program);
        command
            .arg("job-exec")
            .env(PAYLOAD_VAR, serde_json::to_string(payload)?);
        ChildLines::spawn(command).map(|lines| LocalJob { lines })
    }
}

#[derive(Debug)]
pub struct LocalJob {
    lines: ChildLines,
}

impl LocalJob {
    pub async fn next_line(&mut self) -> Option<String> {
        self.lines.next_line().await
    }

    /// SIGTERM, which `job-exec` answers by cancelling its agent and
    /// reporting the round. Returns at once; the round's end arrives on the
    /// stream.
    pub fn cancel(&mut self) {
        self.lines.terminate();
    }

    pub async fn termination(self) -> Termination {
        Termination::Exited(self.lines.exit_code().await)
    }
}
