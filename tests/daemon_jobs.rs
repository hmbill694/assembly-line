//! Jobs through the daemon: submitted, preflighted, claimed, queued under a
//! cap, run on the daemon's runner, and read back with status and logs.

use assembly_line::config::REPO_CONFIG_PATH;
use assembly_line::event::{EventKind, EventLog};
use assembly_line::paths::RepoKey;
use assembly_line::state::JobState;
use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use std::path::{Path, PathBuf};
use support::daemon::{RunningDaemon, wait_for_verdict, wait_until_logged};

mod support;

/// An opted-in repository published to a bare origin, and a daemon on a
/// root beside it running rounds on the local runner.
///
/// `daemon` is declared first so it is dropped — stopped — before `tmp`
/// deletes the root and scratch it is using.
struct Fixture {
    daemon: Option<RunningDaemon>,
    tmp: tempfile::TempDir,
    repo: PathBuf,
    origin: PathBuf,
}

impl Fixture {
    async fn running(script: &str, daemon_args: &[&str], daemon_env: &[(&str, &str)]) -> Self {
        let fx = Self::repository(&format!(
            "verify = \"true\"\n{}",
            support::config_running(script)
        ))
        .await;
        fx.with_daemon(daemon_args, daemon_env)
    }

    async fn repository(config: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let origin = tmp.path().join("origin.git");
        publish_opted_in_repository(&repo, &origin, config).await;
        Fixture {
            daemon: None,
            tmp,
            repo,
            origin,
        }
    }

    /// Start the daemon, with `env` on top of a scratch `TMPDIR` for its
    /// rounds' clones and a `PATH` whose `gh` refuses; a later entry for
    /// the same name wins.
    fn with_daemon(mut self, args: &[&str], env: &[(&str, &str)]) -> Self {
        let scratch = self.tmp.path().join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let path = support::path_where_gh_refuses();
        let env: Vec<(&str, &str)> = [
            ("TMPDIR", scratch.to_str().unwrap()),
            ("PATH", path.as_str()),
        ]
        .into_iter()
        .chain(env.iter().copied())
        .collect();
        self.daemon = Some(RunningDaemon::start(&self.root(), args, &env));
        self
    }

    fn root(&self) -> PathBuf {
        self.tmp.path().join("root")
    }

    fn job_dir(&self, id: u64) -> PathBuf {
        RepoKey::from_remote_url(self.origin.to_str().unwrap())
            .unwrap()
            .jobs_dir(&self.root())
            .join(id.to_string())
    }

    fn events_of(&self, id: u64) -> Vec<EventKind> {
        EventLog::read(self.job_dir(id).join("events.jsonl"))
            .unwrap()
            .into_iter()
            .map(|e| e.kind)
            .collect()
    }

    fn assembly(&self) -> Command {
        let mut cmd = Command::cargo_bin("assembly").unwrap();
        cmd.current_dir(&self.repo)
            .env("PATH", support::path_where_gh_refuses())
            .env("ASSEMBLY_ROOT", self.root());
        cmd
    }

    fn on_origin(&self, args: &[&str]) -> String {
        git_in(&self.origin, args)
    }
}

/// A checkout at `repo` that opts in with `config`, published to a bare
/// `origin`.
async fn publish_opted_in_repository(repo: &Path, origin: &Path, config: &str) {
    support::init_git_repo(repo).await;
    std::fs::create_dir_all(repo.join(".assembly")).unwrap();
    std::fs::write(
        repo.join(REPO_CONFIG_PATH),
        format!("{config}\n[delivery]\nmode = \"none\"\n"),
    )
    .unwrap();
    support::commit_all(repo, "opt in").await.unwrap().unwrap();
    support::add_origin(repo, origin).await;
    support::publish_main(repo).await;
}

fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[tokio::test]
async fn a_submitted_job_is_queued_then_run_by_the_daemon() {
    let fx = Fixture::running("fake-agent.sh", &[], &[]).await;

    fx.assembly()
        .args(["submit", "--prompt", "do the thing"])
        .assert()
        .success()
        .stdout(contains("job 1 queued on al/job-1"));
    let report = wait_for_verdict(&fx.job_dir(1));

    assert_eq!(report.state, JobState::Passed);
    fx.assembly().arg("status").assert().success().stdout(
        contains("job 1: passed (round 1")
            .and(contains("took"))
            .and(contains("branch: al/job-1")),
    );
    fx.assembly()
        .args(["logs", "1"])
        .assert()
        .success()
        .stdout(contains("fake-agent: do the thing"));
}

#[tokio::test]
async fn submit_with_no_daemon_says_how_to_start_one() {
    let fx = Fixture::repository(&support::config_running("fake-agent.sh")).await;

    fx.assembly()
        .args(["submit", "--prompt", "x"])
        .assert()
        .code(2)
        .stderr(contains("assembly daemon"));
}

/// Preflight at submit: every reason at once, and nothing claimed.
#[tokio::test]
async fn a_config_that_cannot_run_is_refused_at_submit_and_claims_nothing() {
    let fx = Fixture::repository("provider = \"ghost\"\nmax_duration = \"soon\"")
        .await
        .with_daemon(&[], &[]);

    fx.assembly()
        .args(["submit", "--prompt", "x"])
        .assert()
        .code(2)
        .stderr(contains("'ghost'").and(contains("max_duration 'soon'")));

    assert_eq!(fx.on_origin(&["branch", "--list", "al/job-*"]), "");
    assert!(!fx.job_dir(1).exists());
}

/// Settings worth flagging do not stop a job, and are said out loud.
#[tokio::test]
async fn a_config_without_verify_is_queued_with_a_warning() {
    let fx = Fixture::repository(&support::config_running("fake-agent.sh"))
        .await
        .with_daemon(&[], &[]);

    fx.assembly()
        .args(["submit", "--prompt", "x"])
        .assert()
        .success()
        .stdout(contains("job 1 queued"))
        .stderr(contains("warn:").and(contains("verify")));
    wait_for_verdict(&fx.job_dir(1));
}

/// Review focus 4.
#[tokio::test]
async fn two_submits_at_once_to_one_repository_get_two_jobs() {
    let fx = Fixture::running("fake-agent.sh", &["--max-jobs", "2"], &[]).await;

    let submits: Vec<std::process::Child> = (0..2)
        .map(|i| {
            std::process::Command::new(env!("CARGO_BIN_EXE_assembly"))
                .current_dir(&fx.repo)
                .env("ASSEMBLY_ROOT", fx.root())
                .args(["submit", "--prompt", &format!("job {i}")])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let outputs: Vec<std::process::Output> = submits
        .into_iter()
        .map(|child| child.wait_with_output().unwrap())
        .collect();

    assert!(
        outputs.iter().all(|out| out.status.success()),
        "{outputs:?}"
    );
    assert_eq!(wait_for_verdict(&fx.job_dir(1)).state, JobState::Passed);
    assert_eq!(wait_for_verdict(&fx.job_dir(2)).state, JobState::Passed);
}

/// The cap, proven by observation: every copy of the probe agent records how
/// many copies were live when it started.
#[tokio::test]
async fn the_daemon_never_runs_more_rounds_at_once_than_its_cap() {
    let probe = tempfile::tempdir().unwrap();
    let probe_dir = probe.path().to_str().unwrap();
    let fx = Fixture::running(
        "counting-agent.sh",
        &["--max-jobs", "2"],
        &[("PROBE_DIR", probe_dir)],
    )
    .await;

    // One after another: each submit is a blocking assertion.
    (0..5).for_each(|i| {
        fx.assembly()
            .args(["submit", "--prompt", &format!("job {i}")])
            .assert()
            .success();
    });
    (1..=5).for_each(|id| {
        assert_eq!(wait_for_verdict(&fx.job_dir(id)).state, JobState::Passed);
    });

    let seen: Vec<u32> = std::fs::read_to_string(probe.path().join("seen"))
        .unwrap()
        .lines()
        .map(|n| n.trim().parse().unwrap())
        .collect();
    assert_eq!(seen.len(), 5);
    assert_eq!(seen.iter().max(), Some(&2), "{seen:?}");
}

#[tokio::test]
async fn a_revise_through_the_daemon_is_the_jobs_next_round() {
    let fx = Fixture::running("revising-agent.sh", &[], &[]).await;
    fx.assembly()
        .args(["submit", "--prompt", "hi"])
        .assert()
        .success();
    wait_for_verdict(&fx.job_dir(1));

    fx.assembly()
        .args(["submit", "--job", "1", "--prompt", "more"])
        .assert()
        .success()
        .stdout(contains("job 1 queued"));
    wait_for_verdict(&fx.job_dir(1));

    fx.assembly()
        .args(["status", "1"])
        .assert()
        .success()
        .stdout(contains("job 1: passed (round 2"));
    assert_eq!(fx.on_origin(&["show", "al/job-1:rounds.txt"]), "hi\nmore");
}

/// P3: every submit, new job or revise, is recorded as the round it asks
/// for, before that round starts.
#[tokio::test]
async fn every_round_asked_for_is_recorded_before_it_starts() {
    let fx = Fixture::running("fake-agent.sh", &[], &[]).await;
    fx.assembly()
        .args(["submit", "--prompt", "add auth"])
        .assert()
        .success();
    wait_for_verdict(&fx.job_dir(1));
    fx.assembly()
        .args(["submit", "--job", "1", "--prompt", "use sessions"])
        .assert()
        .success();
    wait_for_verdict(&fx.job_dir(1));

    let events = fx.events_of(1);
    let asked_and_started: Vec<String> = events
        .iter()
        .filter_map(|kind| match kind {
            EventKind::RoundRequested { prompt, .. } => Some(format!("asked {prompt}")),
            EventKind::RoundStarted { round } => Some(format!("started {round}")),
            _ => None,
        })
        .collect();
    assert_eq!(
        asked_and_started,
        [
            "asked add auth",
            "started 1",
            "asked use sessions",
            "started 2"
        ],
        "{events:?}"
    );
}

/// Counting the `RoundStarted` lines that survive a torn log would hand the
/// next round a number the job already used.
#[tokio::test]
async fn a_revise_is_numbered_past_the_highest_round_recorded() {
    let fx = Fixture::running("fake-agent.sh", &[], &[]).await;
    fx.assembly()
        .args(["submit", "--prompt", "x"])
        .assert()
        .success();
    wait_for_verdict(&fx.job_dir(1));
    let mut log = EventLog::open_append(fx.job_dir(1).join("events.jsonl")).unwrap();
    log.append(EventKind::RoundStarted { round: 3 }).unwrap();
    log.append(EventKind::RoundPassed).unwrap();

    fx.assembly()
        .args(["submit", "--job", "1", "--prompt", "more"])
        .assert()
        .success();

    assert_eq!(wait_for_verdict(&fx.job_dir(1)).rounds, 4);
}

#[tokio::test]
async fn a_revise_of_a_job_still_running_is_refused() {
    let fx = Fixture::running("sleeping-agent.sh", &[], &[]).await;
    fx.assembly()
        .args(["submit", "--prompt", "x"])
        .assert()
        .success();
    wait_until_logged(&fx.job_dir(1), "sleeping-agent:");

    fx.assembly()
        .args(["submit", "--job", "1", "--prompt", "more"])
        .assert()
        .code(2)
        .stderr(contains("already queued or running"));
    fx.assembly().args(["cancel", "1"]).assert().success();
}

#[tokio::test]
async fn cancelling_a_running_job_stops_its_agent_and_records_it() {
    let fx = Fixture::running("sleeping-agent.sh", &[], &[]).await;
    fx.assembly()
        .args(["submit", "--prompt", "x"])
        .assert()
        .success();
    wait_until_logged(&fx.job_dir(1), "sleeping-agent:");

    fx.assembly()
        .args(["cancel", "1"])
        .assert()
        .success()
        .stdout(contains("job 1: cancelling"));

    let report = wait_for_verdict(&fx.job_dir(1));
    assert_eq!(report.state, JobState::Failed);
    assert_eq!(report.detail.as_deref(), Some("cancelled"));
    wait_until_empty(&fx.tmp.path().join("scratch"));
}

/// A job cancelled while it waits for a slot is closed on the spot, and the
/// dispatcher passes over it when its turn comes.
#[tokio::test]
async fn cancelling_a_queued_job_means_it_never_runs() {
    let fx = Fixture::running("sleeping-agent.sh", &["--max-jobs", "1"], &[]).await;
    ["first", "second", "third"].iter().for_each(|prompt| {
        fx.assembly()
            .args(["submit", "--prompt", prompt])
            .assert()
            .success();
    });
    wait_until_logged(&fx.job_dir(1), "sleeping-agent:");

    fx.assembly()
        .args(["cancel", "2"])
        .assert()
        .success()
        .stdout(contains("job 2: cancelled before it started"));
    fx.assembly().args(["cancel", "1"]).assert().success();
    assert_eq!(wait_for_verdict(&fx.job_dir(1)).state, JobState::Failed);
    // The third job is behind the second in line, so once it runs the
    // dispatcher has had its chance to start the second.
    wait_until_logged(&fx.job_dir(3), "sleeping-agent:");
    fx.assembly().args(["cancel", "3"]).assert().success();

    let second = wait_for_verdict(&fx.job_dir(2));
    assert_eq!(second.rounds, 0, "the cancelled job was started anyway");
    assert_eq!(
        second.detail.as_deref(),
        Some("cancelled before it started")
    );
    assert!(
        matches!(
            fx.events_of(2).as_slice(),
            [
                EventKind::RoundRequested { .. },
                EventKind::RoundFailed { .. }
            ]
        ),
        "{:?}",
        fx.events_of(2)
    );
}

/// A job whose round was dispatched but is still reading its config has not
/// started, and a cancel closes it as such rather than leaving it queued
/// with no verdict to come.
#[tokio::test]
async fn cancelling_a_job_still_reading_its_config_closes_it() {
    let fakes = tempfile::tempdir().unwrap();
    let stall = fakes.path().join("stall");
    let path = path_where_git_stalls(fakes.path(), &stall, CONFIG_READ_IN_THE_CACHE);
    let fx = Fixture::running("sleeping-agent.sh", &[], &[("PATH", path.as_str())]).await;
    ["first", "second"].iter().for_each(|prompt| {
        fx.assembly()
            .args(["submit", "--prompt", prompt])
            .assert()
            .success();
    });
    wait_until_logged(&fx.job_dir(1), "sleeping-agent:");
    std::fs::write(&stall, "").unwrap();

    fx.assembly().args(["cancel", "1"]).assert().success();
    wait_until_stalled_times(&stall, 1);
    fx.assembly()
        .args(["cancel", "2"])
        .assert()
        .success()
        .stdout(contains("job 2: cancelling"));

    let second = wait_for_verdict(&fx.job_dir(2));
    std::fs::remove_file(&stall).unwrap();
    assert_eq!(second.rounds, 0);
    assert_eq!(
        second.detail.as_deref(),
        Some("cancelled before it started")
    );
}

/// A job cancelled while queued and then revised is in line twice. Its
/// revise is launched once, and its second place in line gives the slot to
/// the job behind it — here in another repository, so that job's config
/// read does not wait on the first's.
#[tokio::test]
async fn a_job_in_line_twice_is_launched_once() {
    let fakes = tempfile::tempdir().unwrap();
    let stall = fakes.path().join("stall");
    let path = path_where_git_stalls(fakes.path(), &stall, CONFIG_READ_IN_THE_CACHE);
    let fx = Fixture::running(
        "sleeping-agent.sh",
        &["--max-jobs", "2"],
        &[("PATH", path.as_str())],
    )
    .await;
    let other = fx.tmp.path().join("other");
    publish_opted_in_repository(
        &other,
        &fx.tmp.path().join("other.git"),
        &support::config_running("sleeping-agent.sh"),
    )
    .await;
    let other = other.to_str().unwrap();
    ["first", "second", "third"].iter().for_each(|prompt| {
        fx.assembly()
            .args(["submit", "--prompt", prompt])
            .assert()
            .success();
    });
    wait_until_logged(&fx.job_dir(1), "sleeping-agent:");
    wait_until_logged(&fx.job_dir(2), "sleeping-agent:");
    fx.assembly().args(["cancel", "3"]).assert().success();
    fx.assembly()
        .args(["submit", "--job", "3", "--prompt", "again"])
        .assert()
        .success();
    fx.assembly()
        .args(["submit", "--repo", other, "--prompt", "behind"])
        .assert()
        .success();
    std::fs::write(&stall, "").unwrap();

    fx.assembly().args(["cancel", "1"]).assert().success();
    wait_until_stalled_times(&stall, 1);
    fx.assembly().args(["cancel", "2"]).assert().success();
    wait_until_stalled_times(&stall, 2);

    fx.assembly()
        .args(["cancel", "1", "--repo", other])
        .assert()
        .success();
    fx.assembly().args(["cancel", "3"]).assert().success();
    let third = wait_for_verdict(&fx.job_dir(3));
    std::fs::remove_file(&stall).unwrap();
    assert_eq!(third.rounds, 0, "{:?}", fx.events_of(3));
}

/// A round's verdict is recorded before `run` has delivered it. From then
/// on the job is over: a cancel is refused, and a revise is its next round
/// rather than lost behind the one still delivering.
#[tokio::test]
async fn a_job_whose_round_is_still_delivering_has_its_verdict() {
    let fakes = tempfile::tempdir().unwrap();
    let stall = fakes.path().join("stall");
    let path = path_where_git_stalls(fakes.path(), &stall, DELIVERY_CHECK_IN_A_CLONE);
    std::fs::write(&stall, "").unwrap();
    let fx = Fixture::running(
        "fake-agent.sh",
        &["--max-jobs", "2"],
        &[("PATH", path.as_str())],
    )
    .await;
    fx.assembly()
        .args(["submit", "--prompt", "x"])
        .assert()
        .success();
    wait_until_stalled_times(&stall, 1);
    assert_eq!(wait_for_verdict(&fx.job_dir(1)).state, JobState::Passed);

    fx.assembly()
        .args(["cancel", "1"])
        .assert()
        .code(2)
        .stderr(contains("job 1 is not queued or running"));
    fx.assembly()
        .args(["submit", "--job", "1", "--prompt", "more"])
        .assert()
        .success();
    let revised = wait_for_verdict(&fx.job_dir(1));
    std::fs::remove_file(&stall).unwrap();

    assert_eq!(revised.state, JobState::Passed);
    assert_eq!(revised.rounds, 2);
}

#[tokio::test]
async fn cancelling_a_job_that_is_not_running_is_refused() {
    let fx = Fixture::running("fake-agent.sh", &[], &[]).await;
    fx.assembly()
        .args(["submit", "--prompt", "x"])
        .assert()
        .success();
    wait_for_verdict(&fx.job_dir(1));

    fx.assembly()
        .args(["cancel", "1"])
        .assert()
        .code(2)
        .stderr(contains("job 1 is not queued or running"));
}

#[tokio::test]
async fn cancelling_a_job_that_does_not_exist_is_refused() {
    let fx = Fixture::running("fake-agent.sh", &[], &[]).await;

    fx.assembly()
        .args(["cancel", "9"])
        .assert()
        .code(2)
        .stderr(contains("no such job: 9"));
}

#[tokio::test]
async fn cancel_with_no_daemon_says_how_to_start_one() {
    let fx = Fixture::repository(&support::config_running("fake-agent.sh")).await;

    fx.assembly()
        .args(["cancel", "1"])
        .assert()
        .code(2)
        .stderr(contains("assembly daemon"));
}

#[tokio::test]
async fn revising_a_job_that_does_not_exist_is_refused() {
    let fx = Fixture::running("fake-agent.sh", &[], &[]).await;

    fx.assembly()
        .args(["submit", "--job", "9", "--prompt", "more"])
        .assert()
        .code(2)
        .stderr(contains("no such job: 9"));
}

/// A job directory whose log holds no request has nothing to revise it from.
#[tokio::test]
async fn a_job_with_no_recorded_request_cannot_be_revised() {
    let fx = Fixture::running("fake-agent.sh", &[], &[]).await;
    std::fs::create_dir_all(fx.job_dir(1)).unwrap();

    fx.assembly()
        .args(["submit", "--job", "1", "--prompt", "more"])
        .assert()
        .code(2)
        .stderr(contains("no recorded request"));
}

#[tokio::test]
async fn revising_a_job_whose_branch_was_deleted_says_there_is_nothing_to_revise() {
    let fx = Fixture::running("fake-agent.sh", &[], &[]).await;
    fx.assembly()
        .args(["submit", "--prompt", "x"])
        .assert()
        .success();
    wait_for_verdict(&fx.job_dir(1));
    fx.on_origin(&["branch", "-D", "al/job-1"]);
    let events_before = fx.events_of(1);

    fx.assembly()
        .args(["submit", "--job", "1", "--prompt", "try again"])
        .assert()
        .code(2)
        .stderr(contains("job 1 has no branch").and(contains("nothing to revise")));
    assert_eq!(fx.events_of(1), events_before);
}

/// A merged pull request's branch is often deleted, which frees its id on
/// the remote — but not the job's directory under the root.
#[tokio::test]
async fn a_job_whose_branch_was_deleted_keeps_its_directory_to_itself() {
    let fx = Fixture::running("fake-agent.sh", &[], &[]).await;
    fx.assembly()
        .args(["submit", "--prompt", "first job"])
        .assert()
        .success();
    wait_for_verdict(&fx.job_dir(1));
    fx.on_origin(&["branch", "-D", "al/job-1"]);
    let first_events = fx.events_of(1);

    fx.assembly()
        .args(["submit", "--prompt", "second job"])
        .assert()
        .code(2)
        .stderr(contains("still holds that job").and(contains("submit again")));
    assert_eq!(fx.events_of(1), first_events);

    fx.assembly()
        .args(["submit", "--prompt", "second job"])
        .assert()
        .success()
        .stdout(contains("job 2 queued"));
    assert_eq!(wait_for_verdict(&fx.job_dir(2)).state, JobState::Passed);
}

/// `run` can still refuse a job the daemon accepted — its branch deleted
/// while it waited, say. That refusal is `run`'s exit and its stderr, not a
/// frame: the round fails for want of a verdict, and the reason is in the
/// job's log.
#[tokio::test]
async fn a_round_that_run_refuses_fails_with_the_reason_in_its_log() {
    // One slot, held by a first job until its time runs out, so the second
    // waits in the queue long enough to lose its branch.
    let fx = Fixture::repository(&format!(
        "verify = \"true\"\nmax_duration = \"3s\"\n{}",
        support::config_running("sleeping-agent.sh")
    ))
    .await
    .with_daemon(&[], &[]);
    fx.assembly()
        .args(["submit", "--prompt", "first"])
        .assert()
        .success();
    fx.assembly()
        .args(["submit", "--prompt", "second"])
        .assert()
        .success();
    fx.on_origin(&["branch", "-D", "al/job-2"]);
    assert!(
        matches!(
            fx.events_of(2).as_slice(),
            [EventKind::RoundRequested { .. }]
        ),
        "the second job started before its branch was deleted"
    );

    let report = wait_for_verdict(&fx.job_dir(2));

    assert_eq!(report.state, JobState::Failed);
    assert!(
        report
            .detail
            .as_deref()
            .unwrap_or_default()
            .contains("without reporting a verdict: exit 2"),
        "{report:?}"
    );
    fx.assembly()
        .args(["logs", "2"])
        .assert()
        .success()
        .stdout(contains("job 2 has no branch"));
}

#[tokio::test]
async fn a_revise_with_a_missing_prompt_file_starts_no_round() {
    let fx = Fixture::running("fake-agent.sh", &[], &[]).await;
    fx.assembly()
        .args(["submit", "--prompt", "x"])
        .assert()
        .success();
    wait_for_verdict(&fx.job_dir(1));
    let first_events = fx.events_of(1);

    fx.assembly()
        .args(["submit", "--job", "1", "--prompt-file", "gone.md"])
        .assert()
        .code(2)
        .stderr(contains("gone.md"));
    assert_eq!(fx.events_of(1), first_events);
}

/// The tightened invariant: the daemon pins and claims in its own cache;
/// `submit` only lists the remote. Nothing is fetched into the user's `.git`,
/// and nothing lands in its working tree.
#[tokio::test]
async fn the_users_repository_gains_nothing_from_a_job() {
    let fx = Fixture::running("fake-agent.sh", &[], &[]).await;
    let refs_before = git_in(&fx.repo, &["for-each-ref"]);

    fx.assembly()
        .args(["submit", "--prompt", "x"])
        .assert()
        .success();
    wait_for_verdict(&fx.job_dir(1));

    assert!(
        !fx.repo.join(".git/FETCH_HEAD").exists(),
        "something fetched into the user's .git"
    );
    assert_eq!(git_in(&fx.repo, &["for-each-ref"]), refs_before);
    assert_eq!(git_in(&fx.repo, &["status", "--porcelain"]), "");
}

/// Until reattach lands, stopping the daemon ends its rounds — and says so.
/// A job still waiting for a slot is not started on the way out: it stays
/// queued.
#[tokio::test]
async fn stopping_the_daemon_cancels_its_rounds_and_records_why() {
    let mut fx = Fixture::running("sleeping-agent.sh", &[], &[]).await;
    fx.assembly()
        .args(["submit", "--prompt", "x"])
        .assert()
        .success();
    wait_until_logged(&fx.job_dir(1), "sleeping-agent:");
    fx.assembly()
        .args(["submit", "--prompt", "waiting"])
        .assert()
        .success();

    assert!(fx.daemon.take().unwrap().stop().success());

    let report = wait_for_verdict(&fx.job_dir(1));
    assert_eq!(report.state, JobState::Failed);
    assert!(
        report
            .detail
            .as_deref()
            .unwrap_or_default()
            .contains("cancelled"),
        "{report:?}"
    );
    assert!(
        matches!(
            fx.events_of(2).as_slice(),
            [EventKind::RoundRequested { .. }]
        ),
        "{:?}",
        fx.events_of(2)
    );
}

/// A round waiting for its repository's cache, while a submit's fetch into
/// it stalls, must not keep the daemon from stopping — nor be left started
/// with no verdict.
#[tokio::test]
async fn a_stalled_fetch_does_not_hold_up_stopping_the_daemon() {
    let fakes = tempfile::tempdir().unwrap();
    let stall = fakes.path().join("stall");
    let path = path_where_git_stalls(fakes.path(), &stall, FETCH_INTO_THE_CACHE);
    let mut fx = Fixture::repository(&format!(
        "verify = \"true\"\nmax_duration = \"3s\"\n{}",
        support::config_running("sleeping-agent.sh")
    ))
    .await
    .with_daemon(&[], &[("PATH", path.as_str())]);
    fx.assembly()
        .args(["submit", "--prompt", "first"])
        .assert()
        .success();
    fx.assembly()
        .args(["submit", "--prompt", "second"])
        .assert()
        .success();
    std::fs::write(&stall, "").unwrap();
    let mut stalled_submit = std::process::Command::new(env!("CARGO_BIN_EXE_assembly"))
        .current_dir(&fx.repo)
        .env("ASSEMBLY_ROOT", fx.root())
        .args(["submit", "--prompt", "third"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    wait_until_stalled_times(&stall, 1);
    wait_for_verdict(&fx.job_dir(1));
    // Job 2's round has had its chance to take the freed slot.
    std::thread::sleep(std::time::Duration::from_secs(1));

    let stopping = std::time::Instant::now();
    assert!(fx.daemon.take().unwrap().stop().success());
    let _ = stalled_submit.kill();
    let _ = stalled_submit.wait();

    assert!(
        stopping.elapsed() < std::time::Duration::from_secs(15),
        "stopping took {:?}",
        stopping.elapsed()
    );
    assert!(
        matches!(
            fx.events_of(2).as_slice(),
            [EventKind::RoundRequested { .. }]
        ),
        "{:?}",
        fx.events_of(2)
    );
}

/// The config a round reads when it is dispatched comes from the daemon's
/// cache; if that cannot be read, the round fails saying so rather than
/// blaming the runner.
#[tokio::test]
async fn a_round_whose_config_cannot_be_read_at_dispatch_fails_saying_why() {
    let fx = Fixture::repository(&format!(
        "verify = \"true\"\nmax_duration = \"3s\"\n{}",
        support::config_running("sleeping-agent.sh")
    ))
    .await
    .with_daemon(&[], &[]);
    fx.assembly()
        .args(["submit", "--prompt", "first"])
        .assert()
        .success();
    fx.assembly()
        .args(["submit", "--prompt", "second"])
        .assert()
        .success();
    std::fs::remove_dir_all(fx.root().join("repos")).unwrap();

    let report = wait_for_verdict(&fx.job_dir(2));

    assert_eq!(report.state, JobState::Failed);
    let detail = report.detail.unwrap_or_default();
    assert!(
        detail.contains("reading .assembly/config.toml at the job's base"),
        "{detail}"
    );
}

/// Which `git` calls [`path_where_git_stalls`] holds up: a working
/// directory pattern, and a shell test on the arguments.
#[derive(Clone, Copy)]
struct GitCall {
    in_dir: &'static str,
    when: &'static str,
}

/// A fetch into the daemon's repository cache, which holds the cache's lock.
const FETCH_INTO_THE_CACHE: GitCall = GitCall {
    in_dir: "*/root/repos/*",
    when: "[ \"$1\" = fetch ]",
};

/// Reading a job's config from the daemon's repository cache.
const CONFIG_READ_IN_THE_CACHE: GitCall = GitCall {
    in_dir: "*/root/repos/*",
    when: "[ \"$1\" = cat-file ]",
};

/// `run` asking whether the branch it pushed carries work worth delivering —
/// after the round's verdict. Committing asks the same of `..HEAD`, before.
const DELIVERY_CHECK_IN_A_CLONE: GitCall = GitCall {
    in_dir: "*/scratch/*",
    when: "[ \"$1\" = rev-list ] && [ \"${3%..HEAD}\" = \"$3\" ]",
};

/// A `PATH` whose `git`, while `stall` exists, holds up every `call` until
/// `stall` is removed, adding a line to `<stall>.reached` as each one
/// begins to wait.
fn path_where_git_stalls(fakes: &Path, stall: &Path, call: GitCall) -> String {
    let real_git = String::from_utf8(
        std::process::Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    support::fake_cli(
        fakes,
        "git",
        &format!(
            "case \"$(pwd -P)\" in\n  \
               {in_dir}) if {when} && [ -e '{stall}' ]; then\n    \
                 echo stalled >> '{stall}.reached'\n    \
                 while [ -e '{stall}' ]; do sleep 0.1; done\n  \
               fi ;;\n\
             esac\n\
             exec '{}' \"$@\"\n",
            real_git.trim(),
            in_dir = call.in_dir,
            when = call.when,
            stall = stall.display(),
        ),
    );
    format!("{}:{}", fakes.display(), support::path_where_gh_refuses())
}

/// Wait until `times` git calls have stalled on `stall`.
fn wait_until_stalled_times(stall: &Path, times: usize) {
    let reached = stall.with_extension("reached");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::fs::read_to_string(&reached).map_or(0, |log| log.lines().count()) < times {
        assert!(
            std::time::Instant::now() < deadline,
            "fewer than {times} git calls ever stalled on {}",
            stall.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Wait until `dir` holds nothing — a round's clone, say, once the round has
/// cleaned up after itself.
fn wait_until_empty(dir: &Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_some()) {
        assert!(
            std::time::Instant::now() < deadline,
            "{} was never emptied",
            dir.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// A token exported for container runners must not take over a local round,
/// which uses whatever git already authenticates with on this machine.
#[tokio::test]
async fn the_local_runner_keeps_the_hosts_credentials_even_with_a_token_exported() {
    let fx = Fixture::running(
        "credential-reporting-agent.sh",
        &[],
        &[("ASSEMBLY_GIT_TOKEN", "t")],
    )
    .await;

    fx.assembly()
        .args(["submit", "--prompt", "x"])
        .assert()
        .success();
    wait_for_verdict(&fx.job_dir(1));

    fx.assembly()
        .args(["logs", "1"])
        .assert()
        .success()
        .stdout(contains("helpers:").and(contains("x-access-token").not()));
}

#[tokio::test]
async fn unpushed_local_work_is_pointed_out_and_the_remotes_ref_is_used() {
    let fx = Fixture::running("fake-agent.sh", &[], &[]).await;
    let remotes = fx.on_origin(&["rev-parse", "main"]);
    std::fs::write(fx.repo.join("unpushed.txt"), "mine\n").unwrap();
    support::commit_all(&fx.repo, "unpushed")
        .await
        .unwrap()
        .unwrap();

    fx.assembly()
        .args(["submit", "--prompt", "x"])
        .assert()
        .success()
        .stdout(contains("push first"));
    wait_for_verdict(&fx.job_dir(1));

    assert_eq!(fx.on_origin(&["rev-parse", "al/job-1^"]), remotes);
}

/// `--repo` finds a job's state from anywhere — and only with it — and
/// nothing is written to the repository the command was typed in.
#[tokio::test]
async fn a_job_submitted_elsewhere_is_found_by_pointing_the_read_commands_at_it() {
    let fx = Fixture::running("fake-agent.sh", &[], &[]).await;
    let standing_in = support::repo_with_initial_commit().await;
    let at = fx.repo.to_str().unwrap();
    let from_elsewhere = || {
        let mut cmd = Command::cargo_bin("assembly").unwrap();
        cmd.current_dir(standing_in.path())
            .env("ASSEMBLY_ROOT", fx.root());
        cmd
    };

    from_elsewhere()
        .args(["submit", "--repo", at, "--prompt", "x"])
        .assert()
        .success();
    wait_for_verdict(&fx.job_dir(1));

    from_elsewhere()
        .arg("status")
        .assert()
        .code(2)
        .stderr(contains("no 'origin' remote"));
    from_elsewhere()
        .args(["status", "--repo", at])
        .assert()
        .success()
        .stdout(contains("job 1: passed"));
    from_elsewhere()
        .args(["logs", "1", "--repo", at])
        .assert()
        .success()
        .stdout(contains("fake-agent"));
    from_elsewhere()
        .args(["submit", "--job", "1", "--prompt", "again", "--repo", at])
        .assert()
        .success()
        .stdout(contains("job 1 queued"));
    assert_eq!(wait_for_verdict(&fx.job_dir(1)).rounds, 2);
    assert!(!standing_in.path().join(".assembly").exists());
}

/// A daemon whose rounds run in containers cannot take a remote that is
/// only a path on this machine, and says so at submit.
#[tokio::test]
async fn a_remote_that_is_a_local_path_is_refused_by_a_container_daemon() {
    let fx = Fixture::repository(&support::config_running("fake-agent.sh")).await;
    let fakes = fx.tmp.path().join("fakes");
    support::fake_cli(&fakes, "docker", "echo 27.0.0\n");
    let path = format!("{}:{}", fakes.display(), std::env::var("PATH").unwrap());
    let fx = fx.with_daemon(
        &["--runner", "docker"],
        &[
            ("PATH", path.as_str()),
            ("ASSEMBLY_GIT_TOKEN", "t"),
            ("GH_TOKEN", "t"),
        ],
    );

    fx.assembly()
        .args(["submit", "--prompt", "x"])
        .assert()
        .code(2)
        .stderr(contains("is a path on this machine").and(contains("--runner local")));

    assert_eq!(fx.on_origin(&["branch", "--list", "al/job-*"]), "");
}
