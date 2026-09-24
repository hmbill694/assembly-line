//! Running a job as a k8s Job, through the `kubectl` CLI.

use super::child::ChildLines;
use super::{JobSecrets, Runner, RunnerProblem, RunningJob, Termination, job_resource_name};
use crate::payload::{JobPayload, PAYLOAD_VAR};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// How long a pod may sit unscheduled or unpulled before the job fails.
/// `Pending` forever is the likeliest k8s failure there is.
const SCHEDULING_DEADLINE: Duration = Duration::from_mins(10);
const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// How many times in a row reading the pod's status may fail — an API
/// server blip — before the job gives up on it.
const STATUS_ATTEMPTS: u32 = 5;
/// How many reconnects in a row may bring back no new log lines before the
/// stream is treated as broken rather than dropped.
const FRUITLESS_RECONNECTS_ALLOWED: u32 = 5;
/// How long a `kubectl logs -f` must stay connected for its end to count as
/// a dropped connection to a live pod — one that is merely quiet — rather
/// than a stream that cannot be read at all, which ends at once. Such a
/// follow starts the count of fruitless reconnects over.
const LIVE_CONNECTION: Duration = Duration::from_secs(30);
/// How long a finished Job lingers for inspection if its own cleanup fails.
const TTL_AFTER_FINISHED_SECS: u64 = 3600;
/// Covers everything outside the two command limits: the pod's wait to
/// start — up to [`SCHEDULING_DEADLINE`], since a Job's deadline counts from
/// the Job's start, not the pod's — then clone, provisioning and push.
const BACKSTOP_ALLOWANCE_SECS: u64 = 30 * 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PodProgress {
    /// No pod: never created, or deleted.
    Gone,
    Waiting {
        reason: String,
    },
    Running {
        pod: String,
    },
    Finished {
        pod: String,
        exit_code: i32,
        reason: Option<String>,
    },
}

/// Where the Job's pod stands, read from `kubectl get pods -o json`.
#[must_use]
pub fn pod_progress(pods: &Value) -> PodProgress {
    let Some(pod) = pods.pointer("/items/0") else {
        return PodProgress::Gone;
    };
    let name = pod
        .pointer("/metadata/name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let phase = pod
        .pointer("/status/phase")
        .and_then(Value::as_str)
        .unwrap_or("Pending");
    let state = pod.pointer("/status/containerStatuses/0/state");
    let terminated = state.and_then(|s| s.get("terminated"));
    let waiting = state
        .and_then(|s| s.pointer("/waiting/reason"))
        .and_then(Value::as_str);
    // An unscheduled pod has no container to wait on; why it is stuck is on
    // its `PodScheduled` condition instead.
    let unscheduled = pod
        .pointer("/status/conditions")
        .and_then(Value::as_array)
        .and_then(|conditions| {
            conditions
                .iter()
                .find(|c| c["type"] == "PodScheduled" && c["status"] == "False")
        })
        .map(|c| {
            format!(
                "{}: {}",
                c["reason"].as_str().unwrap_or("Unschedulable"),
                c["message"].as_str().unwrap_or_default()
            )
        });

    match (phase, terminated, waiting) {
        (_, Some(t), _) => PodProgress::Finished {
            pod: name,
            exit_code: t
                .get("exitCode")
                .and_then(Value::as_i64)
                .and_then(|c| i32::try_from(c).ok())
                .unwrap_or(-1),
            // "Completed" and "Error" only restate the exit code.
            reason: t
                .get("reason")
                .and_then(Value::as_str)
                .filter(|r| !matches!(*r, "Completed" | "Error"))
                .map(String::from),
        },
        ("Failed", None, _) => PodProgress::Finished {
            pod: name,
            exit_code: -1,
            reason: Some(
                pod.pointer("/status/reason")
                    .and_then(Value::as_str)
                    .unwrap_or("the pod failed")
                    .to_string(),
            ),
        },
        ("Running", None, _) => PodProgress::Running { pod: name },
        (phase, None, reason) => PodProgress::Waiting {
            reason: reason
                .map(String::from)
                .or(unscheduled)
                .unwrap_or_else(|| phase.to_string()),
        },
    }
}

/// The Job's own deadline: a backstop behind the command limits `job-exec`
/// enforces, for a pod that wedges beyond them.
#[must_use]
pub fn active_deadline_secs(payload: &JobPayload) -> Option<u64> {
    payload
        .command_limit_secs
        .map(|limit| 2 * limit + BACKSTOP_ALLOWANCE_SECS)
}

/// A Job that runs `job-exec` once, never retried, with its environment
/// read from the Secret of the same name.
#[must_use]
pub fn job_manifest(name: &str, image: &str, active_deadline_secs: Option<u64>) -> Value {
    // Built, then extended: `json!` has no syntax for an optional key.
    let mut spec = json!({
        "backoffLimit": 0,
        "ttlSecondsAfterFinished": TTL_AFTER_FINISHED_SECS,
        "template": {
            "spec": {
                "restartPolicy": "Never",
                // Nothing in a round talks to the cluster, so the agent is
                // not handed the namespace's API token.
                "automountServiceAccountToken": false,
                "containers": [{
                    "name": "job",
                    "image": image,
                    "command": ["assembly", "job-exec"],
                    "envFrom": [{ "secretRef": { "name": name } }],
                }],
            },
        },
    });
    if let Some(secs) = active_deadline_secs {
        spec["activeDeadlineSeconds"] = json!(secs);
    }
    json!({ "apiVersion": "batch/v1", "kind": "Job", "metadata": { "name": name }, "spec": spec })
}

/// A Secret owned by the Job whose `uid` is `job_uid`, so it is deleted
/// with it.
#[must_use]
pub fn secret_manifest(name: &str, job_uid: &str, vars: &BTreeMap<String, String>) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": name,
            "ownerReferences": [{ "apiVersion": "batch/v1", "kind": "Job", "name": name, "uid": job_uid }],
        },
        "type": "Opaque",
        "stringData": vars,
    })
}

/// `kubectl logs --timestamps` prefixes each line with an RFC 3339
/// timestamp and a space. The timestamp is where a resumed stream restarts.
#[must_use]
pub fn split_timestamp(line: &str) -> (Option<&str>, &str) {
    match line.split_once(' ') {
        Some((stamp, rest)) if chrono::DateTime::parse_from_rfc3339(stamp).is_ok() => {
            (Some(stamp), rest)
        }
        _ => (None, line),
    }
}

/// How far a pod's log has been read. A resumed `kubectl logs` starts at
/// `--since-time`, which is inclusive and truncated to the second, so it
/// replays lines already read; this tells those from lines that are new.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogReadPosition {
    /// The last new line's timestamp as `kubectl` printed it, and how many
    /// new lines carried it.
    read_up_to: Option<(String, usize)>,
    /// How many lines with that timestamp the current stream has shown.
    shown_at_that_stamp: usize,
}

impl LogReadPosition {
    /// Where a resumed stream starts.
    #[must_use]
    pub fn resume_from(&self) -> Option<&str> {
        self.read_up_to.as_ref().map(|(stamp, _)| stamp.as_str())
    }

    /// The position as a new stream starts from it.
    #[must_use]
    pub fn resumed(self) -> Self {
        LogReadPosition {
            shown_at_that_stamp: 0,
            ..self
        }
    }

    /// The position after a line stamped `stamp`, and whether that line is
    /// one not read before.
    #[must_use]
    pub fn after_line(self, stamp: &str) -> (Self, bool) {
        let instant = |s: &str| chrono::DateTime::parse_from_rfc3339(s).ok();
        let first_at = |stamp: &str| LogReadPosition {
            read_up_to: Some((stamp.to_string(), 1)),
            shown_at_that_stamp: 1,
        };
        let Some((last, lines)) = &self.read_up_to else {
            return (first_at(stamp), true);
        };
        match instant(stamp).cmp(&instant(last)) {
            std::cmp::Ordering::Less => (self, false),
            std::cmp::Ordering::Equal => {
                let shown = self.shown_at_that_stamp + 1;
                (
                    LogReadPosition {
                        read_up_to: Some((last.clone(), shown.max(*lines))),
                        shown_at_that_stamp: shown,
                    },
                    shown > *lines,
                )
            }
            std::cmp::Ordering::Greater => (first_at(stamp), true),
        }
    }
}

#[derive(Debug, Clone)]
pub struct KubernetesRunner {
    /// `kubectl`, or a stand-in a test wrote.
    pub program: PathBuf,
    pub image: String,
    /// Where every Job and Secret is created. Required: it is where the
    /// credentials land, so it is never left to a kubeconfig default.
    pub namespace: String,
    /// `kubectl`'s current context when `None`.
    pub context: Option<String>,
    pub scheduling_deadline: Duration,
    pub poll_interval: Duration,
    /// How long a log follow must last to count as a dropped connection to a
    /// live pod, not a stream that cannot be read (`LIVE_CONNECTION`).
    pub live_connection: Duration,
}

impl KubernetesRunner {
    #[must_use]
    pub fn new(image: String, namespace: String, context: Option<String>) -> Self {
        KubernetesRunner {
            program: "kubectl".into(),
            image,
            namespace,
            context,
            scheduling_deadline: SCHEDULING_DEADLINE,
            poll_interval: POLL_INTERVAL,
            live_connection: LIVE_CONNECTION,
        }
    }

    /// Why this runner may not `verb` `resource` in its namespace — `None`
    /// when it may. `Err` when `kubectl` gave no answer at all.
    ///
    /// `kubectl auth can-i` prints `no` and exits 1, so the answer is read
    /// from stdout before the exit status means anything.
    async fn reason_it_may_not(
        &self,
        verb: &str,
        resource: &str,
    ) -> Result<Option<RunnerProblem>, RunnerProblem> {
        let unreachable = |detail: String| RunnerProblem::Unreachable {
            runner: "kubectl",
            detail,
        };
        let out = self
            .kubectl()
            .args(["auth", "can-i", verb, resource])
            .output()
            .await
            .map_err(|e| unreachable(e.to_string()))?;
        match String::from_utf8_lossy(&out.stdout).trim() {
            "yes" => Ok(None),
            answer if answer.starts_with("no") => Ok(Some(RunnerProblem::NotPermitted {
                verb: verb.into(),
                resource: resource.into(),
                namespace: self.namespace.clone(),
            })),
            _ => Err(unreachable(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            )),
        }
    }

    /// `kubectl` pinned to this runner's context and namespace.
    fn kubectl(&self) -> Command {
        let mut command = Command::new(&self.program);
        if let Some(context) = &self.context {
            command.args(["--context", context]);
        }
        command.args(["--namespace", &self.namespace]);
        command
    }
}

/// Run `command`, feeding it `stdin`, and return its stdout.
async fn output_of(mut command: Command, stdin: Option<String>) -> anyhow::Result<String> {
    let mut child = command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            anyhow::anyhow!(
                "spawning `{}`: {e}",
                command.as_std().get_program().display()
            )
        })?;
    if let (Some(body), Some(mut pipe)) = (stdin, child.stdin.take()) {
        pipe.write_all(body.as_bytes()).await?;
    }
    let out = child.wait_with_output().await?;
    anyhow::ensure!(
        out.status.success(),
        "kubectl failed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

impl Runner for KubernetesRunner {
    type Running = KubernetesJob;
    const RUNS_IN_A_CONTAINER: bool = true;

    /// Every permission a round uses, asked up front: one found missing
    /// mid-round would leave a Job running that nothing can follow or delete.
    async fn reasons_it_cannot_run(&self) -> Vec<RunnerProblem> {
        let [
            create_job,
            create_secret,
            list_pods,
            read_logs,
            delete_job,
            delete_secret,
        ] = [
            ("create", "jobs"),
            ("create", "secrets"),
            ("list", "pods"),
            ("get", "pods/log"),
            ("delete", "jobs"),
            ("delete", "secrets"),
        ]
        .map(|(verb, resource)| self.reason_it_may_not(verb, resource));
        let answers = tokio::join!(
            create_job,
            create_secret,
            list_pods,
            read_logs,
            delete_job,
            delete_secret
        );
        let answers = [
            answers.0, answers.1, answers.2, answers.3, answers.4, answers.5,
        ];

        match answers.into_iter().collect::<Result<Vec<_>, _>>() {
            // Every check asks the same `kubectl`: unreachable is one problem.
            Err(unreachable) => vec![unreachable],
            Ok(missing) => missing.into_iter().flatten().collect(),
        }
    }

    async fn launch(
        &self,
        payload: &JobPayload,
        secrets: &JobSecrets,
    ) -> anyhow::Result<KubernetesJob> {
        let mut job = KubernetesJob::named(self.clone(), job_resource_name(payload));
        match job.create_and_follow(payload, secrets).await {
            Ok(()) => Ok(job),
            Err(e) => {
                job.delete_job_and_secret().await;
                Err(e)
            }
        }
    }
}

/// Where a job's log is read from.
#[derive(Debug)]
enum LogStream {
    /// `kubectl logs -f`, which can drop while the pod runs on.
    Following(ChildLines),
    /// One last `kubectl logs` without `-f`, once the pod has finished, for
    /// whatever a dropped stream missed.
    Draining(ChildLines),
    Ended,
}

#[derive(Debug)]
pub struct KubernetesJob {
    runner: KubernetesRunner,
    name: String,
    pod: String,
    stream: LogStream,
    position: LogReadPosition,
    /// Reconnects since the last line not read before, or since a follow
    /// that held for [`KubernetesRunner::live_connection`].
    fruitless_reconnects: u32,
    /// When the current `kubectl logs -f` connected.
    following_since: Option<std::time::Instant>,
    /// The last line with no timestamp — `kubectl`'s own complaint, when a
    /// reconnect fails.
    last_unstamped_line: Option<String>,
    /// Why the stream was abandoned while the pod may still be running.
    stream_abandoned_because: Option<String>,
}

impl KubernetesJob {
    /// The handle for a Job not yet created.
    fn named(runner: KubernetesRunner, name: String) -> Self {
        KubernetesJob {
            runner,
            name,
            pod: String::new(),
            stream: LogStream::Ended,
            position: LogReadPosition::default(),
            fruitless_reconnects: 0,
            following_since: None,
            last_unstamped_line: None,
            stream_abandoned_because: None,
        }
    }

    /// Create the Job and its Secret, wait for its pod, and start following
    /// its log. Every step in one place, so a failure anywhere — a create
    /// that failed after the server acted on it included — is cleaned up
    /// once.
    ///
    /// `create`, not `apply`: the names are unique to the round, and `apply`
    /// would copy the Secret's values into its `last-applied-configuration`
    /// annotation.
    async fn create_and_follow(
        &mut self,
        payload: &JobPayload,
        secrets: &JobSecrets,
    ) -> anyhow::Result<()> {
        let mut create_job = self.runner.kubectl();
        create_job.args(["create", "-f", "-", "-o", "json"]);
        let created: Value = serde_json::from_str(
            &output_of(
                create_job,
                Some(
                    job_manifest(
                        &self.name,
                        &self.runner.image,
                        active_deadline_secs(payload),
                    )
                    .to_string(),
                ),
            )
            .await?,
        )?;
        let uid = created
            .pointer("/metadata/uid")
            .and_then(Value::as_str)
            .filter(|uid| !uid.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!("kubectl create returned no uid for Job {}", self.name)
            })?;

        let vars: BTreeMap<String, String> = secrets
            .vars()
            .clone()
            .into_iter()
            .chain([(PAYLOAD_VAR.to_string(), serde_json::to_string(payload)?)])
            .collect();
        let mut create_secret = self.runner.kubectl();
        create_secret.args(["create", "-f", "-"]);
        output_of(
            create_secret,
            Some(secret_manifest(&self.name, uid, &vars).to_string()),
        )
        .await?;

        self.pod = self.wait_until_started().await?;
        self.stream = LogStream::Following(self.logs(true)?);
        self.following_since = Some(std::time::Instant::now());
        Ok(())
    }

    async fn progress(&self) -> anyhow::Result<PodProgress> {
        let mut command = self.runner.kubectl();
        command.args([
            "get",
            "pods",
            "-l",
            &format!("job-name={}", self.name),
            "-o",
            "json",
        ]);
        let pods: Value = serde_json::from_str(&output_of(command, None).await?)?;
        Ok(pod_progress(&pods))
    }

    /// [`Self::progress`], riding out a few failures in a row — a `kubectl`
    /// error is far likelier an API blip than a verdict on the pod.
    async fn progress_despite_blips(&self) -> anyhow::Result<PodProgress> {
        let mut attempt = 1;
        // A loop: each attempt is a subprocess, retried after a pause.
        loop {
            match self.progress().await {
                Ok(progress) => return Ok(progress),
                Err(e) if attempt >= STATUS_ATTEMPTS => {
                    anyhow::bail!("cannot read the pod's status: {e:#}");
                }
                Err(_) => {
                    attempt += 1;
                    tokio::time::sleep(self.runner.poll_interval).await;
                }
            }
        }
    }

    /// Poll until the pod runs or finishes; fail with the pod's own reason
    /// at the scheduling deadline.
    async fn wait_until_started(&self) -> anyhow::Result<String> {
        let started = std::time::Instant::now();
        // A loop: polling is sequential I/O with an exit on each branch.
        loop {
            let progress = self.progress_despite_blips().await?;
            match (
                progress,
                started.elapsed() >= self.runner.scheduling_deadline,
            ) {
                (PodProgress::Running { pod } | PodProgress::Finished { pod, .. }, _) => {
                    return Ok(pod);
                }
                (PodProgress::Waiting { reason }, true) => {
                    anyhow::bail!("the pod never started: {reason}")
                }
                (PodProgress::Gone, true) => {
                    anyhow::bail!("the pod never started: no pod was created")
                }
                (PodProgress::Waiting { .. } | PodProgress::Gone, false) => {
                    tokio::time::sleep(self.runner.poll_interval).await;
                }
            }
        }
    }

    /// The pod's log from the last timestamp read — the whole log before
    /// any — followed as it grows when `follow` is set.
    fn logs(&self, follow: bool) -> anyhow::Result<ChildLines> {
        let mut command = self.runner.kubectl();
        command.args(["logs", "--timestamps", &format!("pod/{}", self.pod)]);
        if follow {
            command.arg("-f");
        }
        if let Some(since) = self.position.resume_from() {
            command.arg(format!("--since-time={since}"));
        }
        ChildLines::spawn(command)
    }

    /// The line's text, unless a resumed stream is replaying it. A line with
    /// no timestamp is `kubectl`'s own, not the pod's, and never a replay.
    fn text_unless_replayed(&mut self, line: &str) -> Option<String> {
        match split_timestamp(line) {
            (Some(stamp), text) => {
                let (position, unread) = std::mem::take(&mut self.position).after_line(stamp);
                self.position = position;
                if unread {
                    self.fruitless_reconnects = 0;
                }
                unread.then(|| text.to_string())
            }
            (None, text) => {
                self.last_unstamped_line = Some(text.to_string());
                Some(text.to_string())
            }
        }
    }

    /// What to read next once `kubectl logs -f` has ended: a reconnect if
    /// the pod still runs, a last drain if it finished, nothing if it is
    /// gone. `Err` is why the stream has to be abandoned instead.
    async fn stream_after_follow_ended(&self) -> Result<LogStream, String> {
        match self.progress_despite_blips().await {
            Ok(PodProgress::Running { .. })
                if self.fruitless_reconnects >= FRUITLESS_RECONNECTS_ALLOWED =>
            {
                Err(format!(
                    "`kubectl logs` kept ending with no output while the pod ran{}",
                    self.last_unstamped_line
                        .as_deref()
                        .map_or(String::new(), |complaint| format!(": {complaint}"))
                ))
            }
            Ok(PodProgress::Running { .. }) => {
                tokio::time::sleep(self.runner.poll_interval).await;
                self.logs(true)
                    .map(LogStream::Following)
                    .map_err(|e| format!("{e:#}"))
            }
            Ok(PodProgress::Finished { .. }) => Ok(self
                .logs(false)
                .map_or(LogStream::Ended, LogStream::Draining)),
            Ok(PodProgress::Waiting { .. } | PodProgress::Gone) => Ok(LogStream::Ended),
            Err(e) => Err(format!("{e:#}")),
        }
    }

    /// Deleting the Job cascades to its pod and — through its owner
    /// reference — its Secret, but only as the garbage collector gets to it,
    /// and not at all if the Job's own delete failed. The Secret holds
    /// credentials, so it is deleted explicitly too. Nothing to delete is not
    /// a failure worth reporting, so the results are ignored.
    async fn delete_job_and_secret(&self) {
        let mut job = self.runner.kubectl();
        job.args([
            "delete",
            "job",
            &self.name,
            "--ignore-not-found",
            "--wait=false",
        ]);
        let _ = output_of(job, None).await;
        let mut secret = self.runner.kubectl();
        secret.args([
            "delete",
            "secret",
            &self.name,
            "--ignore-not-found",
            "--wait=false",
        ]);
        let _ = output_of(secret, None).await;
    }
}

impl RunningJob for KubernetesJob {
    /// A `kubectl logs -f` that ends while the pod is still running dropped;
    /// start another from the last timestamp. One that ends as the pod
    /// finishes is followed by a last drain from that timestamp. What either
    /// replays is skipped here, by [`LogReadPosition`].
    async fn next_line(&mut self) -> Option<String> {
        // A loop: reconnecting is sequential I/O until a line or the end.
        loop {
            let (lines, draining) = match &mut self.stream {
                LogStream::Following(lines) => (lines, false),
                LogStream::Draining(lines) => (lines, true),
                LogStream::Ended => return None,
            };
            if let Some(line) = lines.next_line().await {
                match self.text_unless_replayed(&line) {
                    Some(text) => return Some(text),
                    None => continue,
                }
            }
            if draining {
                self.stream = LogStream::Ended;
                return None;
            }
            if self
                .following_since
                .is_some_and(|since| since.elapsed() >= self.runner.live_connection)
            {
                self.fruitless_reconnects = 0;
            }
            self.stream = match self.stream_after_follow_ended().await {
                Ok(stream @ LogStream::Following(_)) => {
                    self.fruitless_reconnects += 1;
                    self.following_since = Some(std::time::Instant::now());
                    stream
                }
                Ok(stream) => stream,
                Err(reason) => {
                    self.stream_abandoned_because = Some(reason);
                    LogStream::Ended
                }
            };
            self.position = std::mem::take(&mut self.position).resumed();
        }
    }

    async fn cancel(&mut self) {
        self.delete_job_and_secret().await;
    }

    async fn termination(self) -> Termination {
        let progress = match &self.stream_abandoned_because {
            Some(reason) => Err(reason.clone()),
            None => self
                .progress_despite_blips()
                .await
                .map_err(|e| format!("{e:#}")),
        };
        self.delete_job_and_secret().await;
        match progress {
            Err(reason)
            | Ok(
                PodProgress::Finished {
                    reason: Some(reason),
                    ..
                }
                | PodProgress::Waiting { reason },
            ) => Termination::Killed { reason },
            Ok(PodProgress::Finished { exit_code, .. }) => Termination::Exited(exit_code),
            Ok(PodProgress::Gone) => Termination::Killed {
                reason: "the pod was deleted before it finished".into(),
            },
            Ok(PodProgress::Running { .. }) => Termination::Killed {
                reason: "the log stream ended while the pod was running".into(),
            },
        }
    }
}
