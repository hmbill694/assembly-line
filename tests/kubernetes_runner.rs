use assembly_line::collect::collect;
use assembly_line::event::{Event, EventKind, EventLog};
use assembly_line::frame::FrameWriter;
use assembly_line::git::PinnedRef;
use assembly_line::payload::RoundPayload;
use assembly_line::provider::CommandSpec;
use assembly_line::runner::kubernetes::{
    KubernetesRunner, LogReadPosition, PodProgress, active_deadline_secs, job_manifest,
    pod_progress, secret_manifest, split_timestamp,
};
use assembly_line::runner::{JobSecrets, Runner, RunningRound};
use serde_json::json;
use std::collections::BTreeMap;
use support::{Harness, fake_cli};
use tokio_util::sync::CancellationToken;

mod support;

/// A payload whose only meaningful field is its command limit.
fn payload_with_command_limit(command_limit_secs: Option<u64>) -> RoundPayload {
    RoundPayload {
        job_id: 1.into(),
        round: 1,
        remote_url: "remote-url".into(),
        remote_name: "origin".into(),
        start: PinnedRef {
            name: "main".into(),
            sha: "sha".into(),
        },
        branch: "al/job-1".into(),
        command: CommandSpec {
            program: "agent".into(),
            args: Vec::new(),
        },
        commit_message: "message".into(),
        verify: None,
        command_limit_secs,
        provision_toolchain: true,
    }
}

#[test]
fn a_job_manifest_never_retries_and_reads_its_environment_from_its_secret() {
    let job = job_manifest("al-1-1-abc", "img:1", Some(4200));

    assert_eq!(job["kind"], "Job");
    assert_eq!(job["spec"]["backoffLimit"], 0);
    assert_eq!(job["spec"]["activeDeadlineSeconds"], 4200);
    assert_eq!(job["spec"]["template"]["spec"]["restartPolicy"], "Never");
    let container = &job["spec"]["template"]["spec"]["containers"][0];
    assert_eq!(container["image"], "img:1");
    assert_eq!(container["command"], json!(["assembly", "job-exec"]));
    assert_eq!(container["envFrom"][0]["secretRef"]["name"], "al-1-1-abc");
}

#[test]
fn a_job_pod_is_given_no_service_account_token() {
    let job = job_manifest("al-1-1-abc", "img:1", None);

    assert_eq!(
        job["spec"]["template"]["spec"]["automountServiceAccountToken"],
        false
    );
}

#[test]
fn a_job_with_no_max_duration_has_no_active_deadline() {
    assert!(
        job_manifest("n", "i", None)["spec"]
            .get("activeDeadlineSeconds")
            .is_none()
    );
}

/// Owned by the Job, so deleting the Job — on completion or cancel —
/// deletes the credentials with it.
#[test]
fn a_secret_is_owned_by_its_job_and_carries_the_payload() {
    let vars = BTreeMap::from([("ASSEMBLY_JOB".to_string(), "{}".to_string())]);
    let secret = secret_manifest("al-1-1-abc", "uid-9", &vars);

    assert_eq!(secret["kind"], "Secret");
    assert_eq!(secret["metadata"]["ownerReferences"][0]["uid"], "uid-9");
    assert_eq!(secret["metadata"]["ownerReferences"][0]["kind"], "Job");
    assert_eq!(secret["stringData"]["ASSEMBLY_JOB"], "{}");
}

#[test]
fn the_backstop_deadline_is_twice_the_command_limit_plus_half_an_hour() {
    let payload = payload_with_command_limit(Some(1200));
    assert_eq!(active_deadline_secs(&payload), Some(2 * 1200 + 1800));
    assert_eq!(
        active_deadline_secs(&payload_with_command_limit(None)),
        None
    );
}

fn pods(pod: &serde_json::Value) -> serde_json::Value {
    json!({ "items": [pod] })
}

#[test]
fn pod_progress_reads_waiting_running_and_finished_pods() {
    assert_eq!(pod_progress(&json!({ "items": [] })), PodProgress::Gone);
    assert_eq!(
        pod_progress(&pods(&json!({
            "metadata": { "name": "p" },
            "status": { "phase": "Pending", "containerStatuses": [{ "state": { "waiting": { "reason": "ImagePullBackOff" } } }] }
        }))),
        PodProgress::Waiting {
            reason: "ImagePullBackOff".into()
        }
    );
    assert_eq!(
        pod_progress(&pods(
            &json!({ "metadata": { "name": "p" }, "status": { "phase": "Pending" } })
        )),
        PodProgress::Waiting {
            reason: "Pending".into()
        }
    );
    assert_eq!(
        pod_progress(&pods(&json!({
            "metadata": { "name": "p" },
            "status": { "phase": "Pending", "conditions": [{
                "type": "PodScheduled", "status": "False", "reason": "Unschedulable",
                "message": "0/3 nodes are available: 3 Insufficient cpu."
            }] }
        }))),
        PodProgress::Waiting {
            reason: "Unschedulable: 0/3 nodes are available: 3 Insufficient cpu.".into()
        }
    );
    assert_eq!(
        pod_progress(&pods(
            &json!({ "metadata": { "name": "p" }, "status": { "phase": "Running", "containerStatuses": [{ "state": { "running": {} } }] } })
        )),
        PodProgress::Running { pod: "p".into() }
    );
    assert_eq!(
        pod_progress(&pods(&json!({
            "metadata": { "name": "p" },
            "status": { "phase": "Failed", "containerStatuses": [{ "state": { "terminated": { "exitCode": 137, "reason": "OOMKilled" } } }] }
        }))),
        PodProgress::Finished {
            pod: "p".into(),
            exit_code: 137,
            reason: Some("OOMKilled".into())
        }
    );
    assert_eq!(
        pod_progress(&pods(&json!({
            "metadata": { "name": "p" },
            "status": { "phase": "Succeeded", "containerStatuses": [{ "state": { "terminated": { "exitCode": 0, "reason": "Completed" } } }] }
        }))),
        PodProgress::Finished {
            pod: "p".into(),
            exit_code: 0,
            reason: None
        }
    );
}

#[test]
fn a_timestamped_log_line_splits_into_its_timestamp_and_its_text() {
    assert_eq!(
        split_timestamp("2026-09-21T10:00:00.123456789Z {\"seq\":1}"),
        (Some("2026-09-21T10:00:00.123456789Z"), "{\"seq\":1}")
    );
    assert_eq!(
        split_timestamp("no timestamp here"),
        (None, "no timestamp here")
    );
}

#[test]
fn a_resumed_stream_skips_the_lines_it_replays_and_keeps_the_rest() {
    let read = ["2026-09-21T10:00:00.5Z", "2026-09-21T10:00:01.1Z"]
        .iter()
        .fold(LogReadPosition::default(), |position, stamp| {
            position.after_line(stamp).0
        });
    assert_eq!(read.resume_from(), Some("2026-09-21T10:00:01.1Z"));

    // `--since-time` truncates to the second, so the resumed stream starts
    // with the earlier line too; `.12` is later than `.1`, however it sorts
    // as text.
    let (read, earlier_is_new) = read.resumed().after_line("2026-09-21T10:00:00.5Z");
    let (read, last_is_new) = read.after_line("2026-09-21T10:00:01.1Z");
    let (_, next_is_new) = read.after_line("2026-09-21T10:00:01.12Z");

    assert_eq!(
        (earlier_is_new, last_is_new, next_is_new),
        (false, false, true)
    );
}

#[test]
fn a_second_line_with_the_same_timestamp_is_new_but_replaying_it_is_not() {
    let stamp = "2026-09-21T10:00:00Z";
    let (read, first_is_new) = LogReadPosition::default().after_line(stamp);
    let (read, second_is_new) = read.after_line(stamp);
    let (read, first_replayed_is_new) = read.resumed().after_line(stamp);
    let (read, second_replayed_is_new) = read.after_line(stamp);
    let (_, third_is_new) = read.after_line(stamp);

    assert_eq!(
        (
            first_is_new,
            second_is_new,
            first_replayed_is_new,
            second_replayed_is_new,
            third_is_new
        ),
        (true, true, false, false, true)
    );
}

/// A whole passing round's frames, as `kubectl logs --timestamps` prints
/// them.
fn timestamped_round() -> String {
    let frames = FrameWriter::new(Vec::new());
    frames
        .append_event(EventKind::RoundStarted { round: 1 })
        .unwrap();
    frames.append_output("agent working").unwrap();
    frames.append_event(EventKind::RoundPassed).unwrap();
    String::from_utf8(frames.copy_of_sink())
        .unwrap()
        .lines()
        .enumerate()
        .map(|(i, line)| format!("2026-09-21T10:00:0{i}Z {line}\n"))
        .collect()
}

fn runner(program: std::path::PathBuf) -> KubernetesRunner {
    KubernetesRunner {
        program,
        poll_interval: std::time::Duration::from_millis(10),
        scheduling_deadline: std::time::Duration::from_secs(5),
        ..KubernetesRunner::new("img:1".into(), "factory".into(), None)
    }
}

/// A dropped log stream is resumed, and the frames it replays are not
/// appended twice. The pod runs until the resumed stream has been read.
#[tokio::test]
async fn a_dropped_log_stream_is_resumed_without_duplicating_events() {
    let h = Harness::new().await;
    let fakes = h.scratch_root().with_file_name("fakes");
    let k8s = runner(kubectl_answering(
        &fakes,
        &format!("cat >/dev/null; echo '{CREATED_JOB}'"),
        &format!("if [ -e done ]; then echo '{SUCCEEDED_POD}'; else echo '{RUNNING_POD}'; fi"),
        "case \"$*\" in *since-time*) cat frames; touch done ;; *) head -n 2 frames ;; esac",
    ));

    let (passed, events) = collected_round(&h, &k8s).await;

    assert!(passed, "{events:?}");
    let started = events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::RoundStarted { .. }))
        .count();
    assert_eq!(
        started, 1,
        "a replayed frame was collected twice: {events:?}"
    );

    let argv = std::fs::read_to_string(fakes.join("argv")).unwrap();
    assert!(argv.contains("--namespace factory"), "{argv}");
    assert!(
        argv.contains("--since-time=2026-09-21T10:00:01Z"),
        "resumed from the wrong place: {argv}"
    );
    assert!(
        argv.contains("delete job"),
        "the Job — and its Secret — outlived the round: {argv}"
    );
}

#[tokio::test]
async fn a_pod_that_never_starts_fails_at_the_scheduling_deadline_with_its_reason() {
    let h = Harness::new().await;
    let fakes = h.scratch_root().with_file_name("fakes");
    let program = fake_cli(
        &fakes,
        "kubectl",
        &format!(
            "echo \"$*\" >> {argv}\n\
             case \"$*\" in\n\
               *create*) cat >/dev/null; echo '{{\"metadata\":{{\"uid\":\"u\"}}}}' ;;\n\
               *'get pods'*) echo '{{\"items\":[{{\"metadata\":{{\"name\":\"p\"}},\"status\":{{\"phase\":\"Pending\",\"containerStatuses\":[{{\"state\":{{\"waiting\":{{\"reason\":\"ImagePullBackOff\"}}}}}}]}}}}]}}' ;;\n\
             esac\n",
            argv = fakes.join("argv").display()
        ),
    );
    let k8s = KubernetesRunner {
        scheduling_deadline: std::time::Duration::from_millis(200),
        ..runner(program)
    };

    let err = k8s
        .launch(
            &h.payload_for("x").await,
            &JobSecrets::default(),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err()
        .to_string();

    assert!(err.contains("ImagePullBackOff"), "{err}");
    let argv = std::fs::read_to_string(fakes.join("argv")).unwrap();
    assert!(
        argv.contains("delete job"),
        "the unstarted Job was left behind: {argv}"
    );
}

/// `kubectl auth can-i` answers `no` and exits 1 — the fake does both.
#[tokio::test]
async fn missing_permissions_are_preflight_problems_naming_the_namespace() {
    let tmp = tempfile::tempdir().unwrap();
    let k8s = runner(fake_cli(tmp.path(), "kubectl", "echo no\nexit 1\n"));

    let problems: Vec<String> = k8s
        .reasons_it_cannot_run()
        .await
        .iter()
        .map(ToString::to_string)
        .collect();

    assert_eq!(problems.len(), 6, "{problems:?}");
    assert!(
        problems.iter().all(|p| p.contains("'factory'")),
        "{problems:?}"
    );
    // Following and cleaning up are asked about too, not just creating.
    for needed in ["get pods/log", "delete secrets"] {
        assert!(
            problems.iter().any(|p| p.contains(needed)),
            "{needed}: {problems:?}"
        );
    }
}

#[tokio::test]
async fn an_unreachable_kubectl_is_one_preflight_problem() {
    let tmp = tempfile::tempdir().unwrap();
    let k8s = runner(fake_cli(
        tmp.path(),
        "kubectl",
        "echo 'connection refused' >&2\nexit 1\n",
    ));

    let problems = k8s.reasons_it_cannot_run().await;

    assert!(
        matches!(
            problems.as_slice(),
            [assembly_line::runner::RunnerProblem::Unreachable { runner: "kubectl", detail }]
                if detail == "connection refused"
        ),
        "{problems:?}"
    );
}

const RUNNING_POD: &str = r#"{"items":[{"metadata":{"name":"p"},"status":{"phase":"Running"}}]}"#;
const SUCCEEDED_POD: &str = r#"{"items":[{"metadata":{"name":"p"},"status":{"phase":"Succeeded","containerStatuses":[{"state":{"terminated":{"exitCode":0}}}]}}]}"#;
const CREATED_JOB: &str = r#"{"metadata":{"uid":"u"}}"#;

/// A `kubectl` in `dir` that records its argv and answers `create`, `get
/// pods` and `logs` with the given shell snippets, run from `dir` — where
/// `frames` holds a whole passing round's log.
fn kubectl_answering(
    dir: &std::path::Path,
    create: &str,
    get_pods: &str,
    logs: &str,
) -> std::path::PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("frames"), timestamped_round()).unwrap();
    fake_cli(
        dir,
        "kubectl",
        &format!(
            "cd {dir}\n\
             echo \"$*\" >> argv\n\
             case \"$*\" in\n\
               *create*) {create} ;;\n\
               *'get pods'*) {get_pods} ;;\n\
               *logs*) {logs} ;;\n\
             esac\n",
            dir = dir.display()
        ),
    )
}

/// Launch a round on `k8s` and collect it, within a bound so a stream that
/// never ends fails the test instead of hanging it.
async fn collected_round(h: &Harness, k8s: &KubernetesRunner) -> (bool, Vec<Event>) {
    let paths = h.job_paths();
    let mut log = EventLog::open_append(paths.events()).unwrap();
    let running = k8s
        .launch(
            &h.payload_for("x").await,
            &JobSecrets::default(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let verdict = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        collect(running, &mut log, &paths.log(), 1, CancellationToken::new()),
    )
    .await
    .expect("collection never ended")
    .unwrap();
    (verdict.passed(), EventLog::read(paths.events()).unwrap())
}

/// `kubectl create` can fail after the server made the Job — a timeout, a
/// dropped connection — so a failed create is cleaned up too.
#[tokio::test]
async fn a_job_create_that_fails_still_deletes_the_job_by_name() {
    let h = Harness::new().await;
    let fakes = h.scratch_root().with_file_name("fakes");
    let k8s = runner(kubectl_answering(
        &fakes,
        "cat >/dev/null; echo 'i/o timeout' >&2; exit 1",
        &format!("echo '{RUNNING_POD}'"),
        "cat frames",
    ));

    let err = k8s
        .launch(
            &h.payload_for("x").await,
            &JobSecrets::default(),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err()
        .to_string();

    assert!(err.contains("i/o timeout"), "{err}");
    let argv = std::fs::read_to_string(fakes.join("argv")).unwrap();
    assert!(
        argv.lines().any(|call| call.contains("delete job al-1-1-")),
        "a Job whose create failed was never deleted: {argv}"
    );
}

/// The Job and Secret are created, never applied: `apply` would copy the
/// Secret's values into an annotation.
#[tokio::test]
async fn neither_the_job_nor_its_secret_is_applied() {
    let h = Harness::new().await;
    let fakes = h.scratch_root().with_file_name("fakes");
    let k8s = runner(kubectl_answering(
        &fakes,
        &format!("cat >/dev/null; echo '{CREATED_JOB}'"),
        &format!("echo '{SUCCEEDED_POD}'"),
        "cat frames",
    ));

    collected_round(&h, &k8s).await;

    let argv = std::fs::read_to_string(fakes.join("argv")).unwrap();
    assert_eq!(
        argv.lines()
            .filter(|call| call.contains("create -f -"))
            .count(),
        2,
        "{argv}"
    );
    assert!(!argv.contains("apply"), "{argv}");
}

#[tokio::test]
async fn a_launch_that_fails_after_the_job_exists_deletes_the_job() {
    let h = Harness::new().await;
    let fakes = h.scratch_root().with_file_name("fakes");
    let k8s = runner(kubectl_answering(
        &fakes,
        &format!(
            "input=$(cat); case \"$input\" in \
               *'\"kind\":\"Secret\"'*) echo 'secrets is forbidden' >&2; exit 1 ;; \
               *) echo '{CREATED_JOB}' ;; \
             esac"
        ),
        &format!("echo '{RUNNING_POD}'"),
        "cat frames",
    ));

    let err = k8s
        .launch(
            &h.payload_for("x").await,
            &JobSecrets::default(),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err()
        .to_string();

    assert!(err.contains("secrets is forbidden"), "{err}");
    let argv = std::fs::read_to_string(fakes.join("argv")).unwrap();
    assert!(
        argv.contains("delete job"),
        "a Job with no Secret was left behind: {argv}"
    );
}

/// A failed status check just after the stream drops is an API blip, not
/// the pod's end: the stream is resumed.
#[tokio::test]
async fn a_status_check_that_fails_once_does_not_end_the_stream() {
    let h = Harness::new().await;
    let fakes = h.scratch_root().with_file_name("fakes");
    let k8s = runner(kubectl_answering(
        &fakes,
        &format!("cat >/dev/null; echo '{CREATED_JOB}'"),
        &format!(
            "if [ -e done ]; then echo '{SUCCEEDED_POD}'; \
             elif [ -e dropped ] && [ ! -e blipped ]; then touch blipped; echo 'connection refused' >&2; exit 1; \
             else echo '{RUNNING_POD}'; fi"
        ),
        "case \"$*\" in *since-time*) cat frames; touch done ;; *) head -n 2 frames; touch dropped ;; esac",
    ));

    let (passed, events) = collected_round(&h, &k8s).await;

    assert!(fakes.join("blipped").exists(), "the blip never happened");
    assert!(passed, "{events:?}");
}

/// A stream that drops as the pod finishes loses nothing: a last drain
/// reads what it missed, verdict included.
#[tokio::test]
async fn a_stream_dropped_as_the_pod_finishes_is_drained() {
    let h = Harness::new().await;
    let fakes = h.scratch_root().with_file_name("fakes");
    let k8s = runner(kubectl_answering(
        &fakes,
        &format!("cat >/dev/null; echo '{CREATED_JOB}'"),
        &format!("echo '{SUCCEEDED_POD}'"),
        "case \"$*\" in *since-time*) cat frames ;; *) head -n 2 frames ;; esac",
    ));

    let (passed, events) = collected_round(&h, &k8s).await;

    assert!(
        events
            .iter()
            .any(|e| matches!(e.kind, EventKind::RoundPassed)),
        "{events:?}"
    );
    assert!(passed, "{events:?}");
    let argv = std::fs::read_to_string(fakes.join("argv")).unwrap();
    assert!(
        argv.lines()
            .any(|call| call.contains("--since-time=2026-09-21T10:00:01Z") && !call.contains(" -f")),
        "no drain from the last timestamp: {argv}"
    );
}

/// `kubectl logs` that fails at once, every time, while the pod runs — no
/// permission to read logs, say — ends the round naming the failure rather
/// than reconnecting for the pod's whole life.
#[tokio::test]
async fn a_log_stream_that_never_yields_ends_the_round_with_kubectls_complaint() {
    let h = Harness::new().await;
    let fakes = h.scratch_root().with_file_name("fakes");
    let k8s = runner(kubectl_answering(
        &fakes,
        &format!("cat >/dev/null; echo '{CREATED_JOB}'"),
        &format!("echo '{RUNNING_POD}'"),
        "echo 'Error from server (Forbidden): cannot get pods/log' >&2; exit 1",
    ));

    let (passed, events) = collected_round(&h, &k8s).await;

    assert!(!passed);
    assert!(
        events.iter().any(|e| matches!(
            &e.kind,
            EventKind::RoundFailed { reason } if reason.contains("Forbidden")
        )),
        "{events:?}"
    );
    let argv = std::fs::read_to_string(fakes.join("argv")).unwrap();
    assert!(argv.contains("delete job"), "{argv}");
}

/// Every reconnect replays the one line the pod ever printed — `--since-time`
/// is inclusive — so the stream is fruitless, not busy, and the round ends;
/// and the line lands in the log once.
#[tokio::test]
async fn a_log_stream_that_only_replays_ends_the_round_and_logs_the_line_once() {
    let h = Harness::new().await;
    let fakes = h.scratch_root().with_file_name("fakes");
    let k8s = runner(kubectl_answering(
        &fakes,
        &format!("cat >/dev/null; echo '{CREATED_JOB}'"),
        &format!("echo '{RUNNING_POD}'"),
        "echo '2026-09-21T10:00:00Z job-exec: something unframed'",
    ));

    let (passed, events) = collected_round(&h, &k8s).await;

    assert!(!passed);
    assert!(
        events.iter().any(|e| matches!(
            &e.kind,
            EventKind::RoundFailed { reason } if reason.contains("kept ending with no output")
        )),
        "{events:?}"
    );
    let log = std::fs::read_to_string(h.job_paths().log()).unwrap();
    assert_eq!(log.matches("something unframed").count(), 1, "{log}");
}

/// An agent can be quiet for longer than a log connection survives. Each
/// follow here holds for a while, replays the one line and drops — a live
/// pod on a flaky connection, not a stream that cannot be read — so however
/// many times it reconnects, the round is collected once the pod finishes.
#[tokio::test]
async fn a_quiet_pod_whose_connection_keeps_dropping_is_not_abandoned() {
    let h = Harness::new().await;
    let fakes = h.scratch_root().with_file_name("fakes");
    let k8s = KubernetesRunner {
        live_connection: std::time::Duration::from_millis(100),
        ..runner(kubectl_answering(
            &fakes,
            &format!("cat >/dev/null; echo '{CREATED_JOB}'"),
            &format!(
                "n=$(( $(cat polls 2>/dev/null || echo 0) + 1 )); echo $n > polls\n\
                 if [ $n -gt 12 ]; then echo '{SUCCEEDED_POD}'; else echo '{RUNNING_POD}'; fi"
            ),
            "case \"$*\" in\n\
               *-f*) echo '2026-09-21T09:59:59Z thinking'; sleep 0.2 ;;\n\
               *) cat frames ;;\n\
             esac",
        ))
    };

    let (passed, events) = collected_round(&h, &k8s).await;

    assert!(passed, "{events:?}");
    let follows = std::fs::read_to_string(fakes.join("argv"))
        .unwrap()
        .lines()
        .filter(|l| l.contains(" logs ") && l.split(' ').any(|arg| arg == "-f"))
        .count();
    assert!(
        follows > 6,
        "the pod should have outlasted the fruitless-reconnect allowance: {follows} follows"
    );
}

const PENDING_POD: &str = r#"{"items":[{"metadata":{"name":"p"},"status":{"phase":"Pending"}}]}"#;

/// Ctrl-C while the pod is still `Pending` ends the launch at once, rather
/// than after the scheduling deadline, and takes the Job and its Secret
/// down with it.
#[tokio::test]
async fn cancelling_a_launch_whose_pod_is_pending_deletes_the_job_and_its_secret() {
    let h = Harness::new().await;
    let fakes = h.scratch_root().with_file_name("fakes");
    let k8s = KubernetesRunner {
        scheduling_deadline: std::time::Duration::from_secs(60),
        ..runner(kubectl_answering(
            &fakes,
            &format!("cat >/dev/null; echo '{CREATED_JOB}'"),
            &format!("echo '{PENDING_POD}'"),
            "cat frames",
        ))
    };
    let cancel = CancellationToken::new();
    let payload = h.payload_for("x").await;
    let interrupt = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        interrupt.cancel();
    });

    let err = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        k8s.launch(&payload, &JobSecrets::default(), &cancel),
    )
    .await
    .expect("the launch waited out the scheduling deadline despite the cancel")
    .unwrap_err()
    .to_string();

    assert!(err.contains("cancelled before the pod started"), "{err}");
    let argv = std::fs::read_to_string(fakes.join("argv")).unwrap();
    assert!(
        argv.contains("delete job"),
        "the Job was left behind: {argv}"
    );
    assert!(
        argv.contains("delete secret"),
        "the credential Secret was left behind: {argv}"
    );
}

/// Cancelling a job whose pod is running deletes both the Job and the
/// Secret carrying its credentials.
#[tokio::test]
async fn cancelling_a_running_job_deletes_the_job_and_its_secret() {
    let h = Harness::new().await;
    let fakes = h.scratch_root().with_file_name("fakes");
    let k8s = runner(kubectl_answering(
        &fakes,
        &format!("cat >/dev/null; echo '{CREATED_JOB}'"),
        &format!("echo '{RUNNING_POD}'"),
        "cat frames",
    ));

    let mut running = k8s
        .launch(
            &h.payload_for("x").await,
            &JobSecrets::default(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    running.cancel().await;

    let argv = std::fs::read_to_string(fakes.join("argv")).unwrap();
    assert!(argv.contains("delete job"), "{argv}");
    assert!(argv.contains("delete secret"), "{argv}");
}
