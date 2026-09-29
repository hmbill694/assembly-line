//! Running a round in a container, through the `docker` CLI.

use super::child::ChildLines;
use super::{
    JobSecrets, LaunchSpec, RoundHandle, Runner, RunnerProblem, RunningRound, Termination,
};
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
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

/// `docker create`'s arguments. Only *names* of environment variables
/// appear: `-e NAME` takes the value from the client's environment, so no
/// secret is ever visible on a command line.
///
/// No `--rm`: a container that exits non-zero is inspected first, to learn
/// whether it was OOM-killed, and removed afterwards.
#[must_use]
pub fn docker_create_args(
    image: &str,
    container: &str,
    env_names: &[&str],
    args: &[String],
) -> Vec<String> {
    ["create", "--name", container]
        .into_iter()
        .map(String::from)
        .chain(
            env_names
                .iter()
                .flat_map(|name| ["-e".to_string(), (*name).to_string()]),
        )
        .chain([image.to_string(), "assembly".to_string()])
        .chain(args.iter().cloned())
        .collect()
}

impl Runner for DockerRunner {
    type Running = DockerRound;
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

    /// Create the container, then start it, then follow its log. `docker
    /// create` pulls the image first, which can take minutes; a cancel
    /// meanwhile abandons the client and removes whatever it had created, so
    /// no container is left to start on its own later.
    async fn launch(
        &self,
        spec: &LaunchSpec,
        secrets: &JobSecrets,
        cancel: &CancellationToken,
    ) -> anyhow::Result<DockerRound> {
        let names = secrets.names();
        let env_names: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut create = self.docker();
        create
            .args(docker_create_args(
                &self.image,
                &spec.name,
                &env_names,
                &spec.args,
            ))
            .envs(secrets.vars());

        let created = tokio::select! {
            created = succeeded(create) => created,
            () = cancel.cancelled() => {
                remove_container(&self.program, &spec.name).await;
                anyhow::bail!("cancelled before the container started");
            }
        };
        let mut start = self.docker();
        start.args(["start", &spec.name]);
        let started = match created {
            Ok(_) => succeeded(start).await,
            Err(e) => Err(e),
        };
        if let Err(e) = started {
            remove_container(&self.program, &spec.name).await;
            return Err(e);
        }
        DockerRound::following(self.program.clone(), spec.name.clone())
    }

    /// Another `docker logs -f`, which replays the container's log from its
    /// start; the collector drops what it already has.
    fn reattach(
        &self,
        handle: &RoundHandle,
    ) -> impl Future<Output = anyhow::Result<DockerRound>> + Send {
        std::future::ready(match handle {
            RoundHandle::Docker { container } => {
                DockerRound::following(self.program.clone(), container.clone())
            }
            other => Err(other.launched_by_another_runner("docker")),
        })
    }
}

impl DockerRunner {
    /// `docker`, killed if the future driving it is dropped.
    fn docker(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.stdin(Stdio::null()).kill_on_drop(true);
        command
    }
}

/// `command`'s output, or an error carrying its stderr when it fails.
async fn succeeded(mut command: Command) -> anyhow::Result<Output> {
    let out = command.output().await.map_err(|e| {
        anyhow::anyhow!(
            "spawning `{}`: {e}",
            command.as_std().get_program().display()
        )
    })?;
    anyhow::ensure!(
        out.status.success(),
        "`docker {}` failed: {}",
        command
            .as_std()
            .get_args()
            .next()
            .map(|verb| verb.to_string_lossy())
            .unwrap_or_default(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(out)
}

/// Remove the container, running or not. Nothing to remove is not a failure
/// worth reporting, so the result is ignored.
async fn remove_container(program: &Path, container: &str) {
    let _ = Command::new(program)
        .args(["rm", "-f", container])
        .output()
        .await;
}

#[derive(Debug)]
pub struct DockerRound {
    lines: ChildLines,
    container: String,
    program: PathBuf,
    /// The `docker stop` a cancel started, still to be reaped.
    stopping: Option<tokio::process::Child>,
}

impl DockerRound {
    /// `docker logs -f` on `container`, from the start of its log.
    fn following(program: PathBuf, container: String) -> anyhow::Result<DockerRound> {
        let mut logs = Command::new(&program);
        logs.args(["logs", "-f", &container]);
        ChildLines::spawn(logs).map(|lines| DockerRound {
            lines,
            container,
            program,
            stopping: None,
        })
    }

    /// The container's exit code, once it has exited, from `docker wait`.
    async fn exit_code(program: &Path, container: &str) -> Result<i32, String> {
        let mut wait = Command::new(program);
        wait.args(["wait", container]);
        let out = succeeded(wait).await.map_err(|e| format!("{e:#}"))?;
        let printed = String::from_utf8_lossy(&out.stdout);
        printed
            .trim()
            .parse()
            .map_err(|_| format!("`docker wait` printed no exit code: {printed}"))
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

impl RunningRound for DockerRound {
    async fn next_line(&mut self) -> Option<String> {
        self.lines.next_line().await
    }

    /// `docker stop` sends SIGTERM first, as the other runners do, so `run`
    /// can stop its agent, keep the work and report the round; SIGKILL only
    /// once `STOP_GRACE_SECS` have passed.
    ///
    /// `docker stop` is started, not waited on: it returns only once the
    /// container has exited, and the caller keeps reading the container's
    /// log meanwhile, which it can only do once this returns. It is reaped,
    /// and the container removed, at [`RunningRound::termination`].
    async fn cancel(&mut self) {
        self.stopping = Command::new(&self.program)
            // `-t`: its long form was `--time`, now `--timeout`, and only
            // the short flag is spelled the same in every docker release.
            .args(["stop", "-t", &STOP_GRACE_SECS.to_string(), &self.container])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok();
    }

    async fn termination(self) -> Termination {
        let DockerRound {
            lines,
            container,
            program,
            stopping,
        } = self;
        lines.exit_code().await;
        if let Some(mut stop) = stopping {
            let _ = stop.wait().await;
        }
        let termination = match DockerRound::exit_code(&program, &container).await {
            Err(reason) => Termination::Killed { reason },
            Ok(code) if code != 0 && DockerRound::was_oom_killed(&program, &container).await => {
                Termination::Killed {
                    reason: "out of memory".into(),
                }
            }
            Ok(code) => Termination::Exited(code),
        };
        remove_container(&program, &container).await;
        termination
    }

    fn handle(&self) -> RoundHandle {
        RoundHandle::Docker {
            container: self.container.clone(),
        }
    }
}
