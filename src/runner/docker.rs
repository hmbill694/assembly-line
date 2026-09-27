//! Running a job in a container, through the `docker` CLI.

use super::child::ChildLines;
use super::{JobSecrets, Runner, RunnerProblem, RunningJob, Termination, job_resource_name};
use crate::payload::{JobPayload, PAYLOAD_VAR};
use std::path::{Path, PathBuf};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

/// How long a cancelled container has to wind its round down — push
/// included — before `docker stop` kills it. k8s's own default grace.
const STOP_GRACE_SECS: u32 = 30;

#[derive(Debug, Clone)]
pub struct DockerRunner {
    /// `docker`, or a stand-in a test wrote.
    pub program: PathBuf,
    pub image: String,
}

impl DockerRunner {
    #[must_use]
    pub fn new(image: String) -> Self {
        DockerRunner {
            program: "docker".into(),
            image,
        }
    }
}

/// `docker run`'s arguments. Only *names* of environment variables appear:
/// `-e NAME` takes the value from the client's environment, so no secret is
/// ever visible on a command line.
///
/// No `--rm`: a container that exits non-zero is inspected first, to learn
/// whether it was OOM-killed, and removed afterwards.
#[must_use]
pub fn docker_run_args(image: &str, container: &str, env_names: &[&str]) -> Vec<String> {
    ["run", "--name", container]
        .into_iter()
        .map(String::from)
        .chain(
            env_names
                .iter()
                .flat_map(|name| ["-e".to_string(), (*name).to_string()]),
        )
        .chain([
            image.to_string(),
            "assembly".to_string(),
            "job-exec".to_string(),
        ])
        .collect()
}

impl Runner for DockerRunner {
    type Running = DockerJob;
    const RUNS_IN_A_CONTAINER: bool = true;

    async fn reasons_it_cannot_run(&self) -> Vec<RunnerProblem> {
        match Command::new(&self.program)
            .args(["version", "--format", "{{.Server.Version}}"])
            .output()
            .await
        {
            Ok(out) if out.status.success() => Vec::new(),
            Ok(out) => vec![RunnerProblem::Unreachable {
                runner: "docker",
                detail: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            }],
            Err(e) => vec![RunnerProblem::Unreachable {
                runner: "docker",
                detail: e.to_string(),
            }],
        }
    }

    /// Spawning the client does not wait on it, so the launch is ready at
    /// once, with nothing for `cancel` to interrupt.
    fn launch(
        &self,
        payload: &JobPayload,
        secrets: &JobSecrets,
        _cancel: &CancellationToken,
    ) -> impl Future<Output = anyhow::Result<DockerJob>> + Send {
        std::future::ready(self.spawn_docker_run(payload, secrets))
    }
}

impl DockerRunner {
    /// `docker run` as a child of this process, carrying the payload and
    /// `secrets` in its environment.
    fn spawn_docker_run(
        &self,
        payload: &JobPayload,
        secrets: &JobSecrets,
    ) -> anyhow::Result<DockerJob> {
        let container = job_resource_name(payload);
        let names = secrets.names();
        let env_names: Vec<&str> = std::iter::once(PAYLOAD_VAR)
            .chain(names.iter().map(String::as_str))
            .collect();

        let mut command = Command::new(&self.program);
        command
            .args(docker_run_args(&self.image, &container, &env_names))
            .env(PAYLOAD_VAR, serde_json::to_string(payload)?)
            .envs(secrets.vars());

        ChildLines::spawn(command).map(|lines| DockerJob {
            lines,
            container,
            program: self.program.clone(),
            stopping: None,
        })
    }
}

#[derive(Debug)]
pub struct DockerJob {
    lines: ChildLines,
    container: String,
    program: PathBuf,
    /// The `docker stop` a cancel started, still to be reaped.
    stopping: Option<tokio::process::Child>,
}

impl DockerJob {
    /// Remove the container, running or not. Nothing to remove is not a
    /// failure worth reporting, so the result is ignored.
    async fn remove_container(program: &Path, container: &str) {
        let _ = Command::new(program)
            .args(["rm", "-f", container])
            .output()
            .await;
    }

    /// Whether the kernel killed the container for exceeding its memory.
    async fn was_oom_killed(program: &Path, container: &str) -> bool {
        Command::new(program)
            .args(["inspect", "--format", "{{.State.OOMKilled}}", container])
            .output()
            .await
            .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "true")
    }
}

impl RunningJob for DockerJob {
    async fn next_line(&mut self) -> Option<String> {
        self.lines.next_line().await
    }

    /// Killing the `docker` client does not stop the container; `docker
    /// stop` does, and the client exits with it. SIGTERM first, as the other
    /// runners send it, so `job-exec` can stop its agent, keep the work and
    /// report the round; SIGKILL only once `STOP_GRACE_SECS` have passed.
    ///
    /// `docker stop` is started, not waited on: it returns only once the
    /// container has exited, and a container still printing as it winds down
    /// blocks on a full pipe unless its output keeps being read — which the
    /// caller can only do once this returns. It is reaped, and the container
    /// removed, at [`RunningJob::termination`].
    ///
    /// A cancel that arrives before the container exists — the image still
    /// pulling — finds nothing to stop and is lost. Ctrl-C, the only cancel
    /// today, also reaches the `docker run` client in the foreground process
    /// group and aborts it; a daemon's cancel (F3) will need the container
    /// created in `launch`, before this can be called.
    async fn cancel(&mut self) {
        self.stopping = Command::new(&self.program)
            // `-t`: its long form was `--time`, now `--timeout`, and only
            // the short flag is spelled the same in every docker release.
            .args(["stop", "-t", &STOP_GRACE_SECS.to_string(), &self.container])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok();
    }

    async fn termination(self) -> Termination {
        let DockerJob {
            lines,
            container,
            program,
            stopping,
        } = self;
        let exit_code = lines.exit_code().await;
        if let Some(mut stop) = stopping {
            let _ = stop.wait().await;
        }
        let termination = match exit_code {
            code if code != 0 && DockerJob::was_oom_killed(&program, &container).await => {
                Termination::Killed {
                    reason: "out of memory".into(),
                }
            }
            code => Termination::Exited(code),
        };
        DockerJob::remove_container(&program, &container).await;
        termination
    }
}
