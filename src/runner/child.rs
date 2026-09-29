//! A child process's stdout and stderr, merged into one stream of lines.
//!
//! k8s merges the two anyway, so every runner treats them as one stream and
//! lets the frame parser tell `run`'s frames from anything else.

use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

#[derive(Debug)]
pub struct ChildLines {
    child: Child,
    lines: mpsc::Receiver<String>,
}

impl ChildLines {
    /// # Errors
    ///
    /// Returns an error if the program cannot be spawned.
    pub fn spawn(mut command: Command) -> anyhow::Result<Self> {
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                anyhow::anyhow!(
                    "spawning `{}`: {e}",
                    command.as_std().get_program().display()
                )
            })?;

        let (sender, lines) = mpsc::channel(256);
        forward_lines(child.stdout.take(), sender.clone());
        forward_lines(child.stderr.take(), sender);
        Ok(ChildLines { child, lines })
    }

    /// The next line on either stream, or `None` once both have closed.
    pub async fn next_line(&mut self) -> Option<String> {
        self.lines.recv().await
    }

    /// Wait for the child, returning its exit code, or -1 when a signal
    /// ended it.
    pub async fn exit_code(mut self) -> i32 {
        self.child
            .wait()
            .await
            .ok()
            .and_then(|status| status.code())
            .unwrap_or(-1)
    }
}

fn forward_lines(
    source: Option<impl AsyncRead + Unpin + Send + 'static>,
    sender: mpsc::Sender<String>,
) {
    let Some(source) = source else { return };
    tokio::spawn(async move {
        let mut segments = BufReader::new(source).split(b'\n');
        while let Ok(Some(segment)) = segments.next_segment().await {
            if sender
                .send(String::from_utf8_lossy(&segment).into_owned())
                .await
                .is_err()
            {
                return;
            }
        }
    });
}
