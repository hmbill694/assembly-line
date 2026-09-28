use assembly_line::event::{Event, EventKind};
use assembly_line::git::PinnedRef;
use assembly_line::report::JobReport;
use assembly_line::state::JobState;
use chrono::{DateTime, TimeDelta, Utc};

/// Build a timeline where each event is stamped at a fixed offset in seconds.
fn timeline(entries: Vec<(i64, EventKind)>) -> Vec<Event> {
    let origin: DateTime<Utc> = Utc::now();
    entries
        .into_iter()
        .map(|(offset, kind)| Event {
            at: origin + TimeDelta::seconds(offset),
            kind,
        })
        .collect()
}

fn committed(files: usize, insertions: usize, deletions: usize) -> EventKind {
    EventKind::RoundCommitted {
        sha: "abc".into(),
        files,
        insertions,
        deletions,
    }
}

fn state_after(kinds: Vec<EventKind>) -> JobState {
    let events = timeline(kinds.into_iter().map(|kind| (0, kind)).collect());
    JobReport::from_events(1, &events).state
}

fn pushed() -> EventKind {
    EventKind::BranchPushed {
        branch: "al/job-1".into(),
        pushed_to: "origin".into(),
    }
}

#[test]
fn a_job_state_starts_pending() {
    assert_eq!(JobState::default(), JobState::Pending);
}

#[test]
fn starting_and_passing_moves_a_job_through_running_to_passed() {
    let started = vec![EventKind::RoundStarted { round: 1 }];
    assert_eq!(state_after(started.clone()), JobState::Running);

    let finished = [started, vec![EventKind::RoundPassed]].concat();
    assert_eq!(state_after(finished), JobState::Passed);
}

#[test]
fn a_failed_job_is_recorded_as_failed() {
    let st = state_after(vec![EventKind::RoundFailed {
        reason: "exit 1".into(),
    }]);

    assert_eq!(st, JobState::Failed);
}

#[test]
fn a_commit_is_progress_not_completion() {
    let through_commit = vec![EventKind::RoundStarted { round: 1 }, committed(2, 10, 1)];
    assert_eq!(
        state_after(through_commit.clone()),
        JobState::Running,
        "a commit is not completion"
    );

    let finished = [through_commit, vec![EventKind::RoundPassed]].concat();
    assert_eq!(state_after(finished), JobState::Passed);
}

/// A round's branch is pushed before its verdict is reached, so pushing must
/// not decide the verdict either way.
#[test]
fn pushing_a_branch_does_not_decide_the_verdict() {
    let st = state_after(vec![
        EventKind::RoundStarted { round: 1 },
        pushed(),
        EventKind::RoundFailed {
            reason: "exit 3".into(),
        },
    ]);

    assert_eq!(st, JobState::Failed);
}

#[test]
fn a_revise_round_puts_a_finished_job_back_into_running() {
    let st = state_after(vec![
        EventKind::RoundStarted { round: 1 },
        EventKind::RoundPassed,
        EventKind::RoundStarted { round: 2 },
    ]);

    assert_eq!(st, JobState::Running);
}

#[test]
fn the_final_state_is_reconstructed_from_the_log_alone() {
    let st = state_after(vec![
        EventKind::RoundStarted { round: 1 },
        committed(1, 1, 0),
        pushed(),
        EventKind::RoundPassed,
    ]);

    assert_eq!(st, JobState::Passed);
}

#[test]
fn every_state_has_a_label() {
    assert_eq!(JobState::Pending.label(), "pending");
    assert_eq!(JobState::Running.label(), "running");
    assert_eq!(JobState::Passed.label(), "passed");
    assert_eq!(JobState::Failed.label(), "failed");
}

#[test]
fn summarizes_state_rounds_duration_and_detail() {
    let events = timeline(vec![
        (0, EventKind::RoundStarted { round: 1 }),
        (
            9,
            EventKind::RoundFailed {
                reason: "exit 1".into(),
            },
        ),
    ]);

    let report = JobReport::from_events(7, &events);

    assert_eq!(report.id, 7);
    assert_eq!(report.state, JobState::Failed);
    assert_eq!(report.rounds, 1);
    assert_eq!(report.duration.unwrap().as_secs(), 9);
    assert_eq!(report.detail.as_deref(), Some("exit 1"));
}

#[test]
fn a_job_with_no_events_is_pending() {
    let report = JobReport::from_events(1, &[]);

    assert_eq!(report.state, JobState::Pending);
    assert_eq!(report.rounds, 1);
    assert!(report.duration.is_none());
    assert!(report.diff.is_none());
    assert!(report.branch.is_none());
}

#[test]
fn a_revised_job_reports_its_last_round() {
    let events = timeline(vec![
        (0, EventKind::RoundStarted { round: 1 }),
        (10, EventKind::RoundPassed),
        (10, EventKind::RoundStarted { round: 2 }),
        (13, EventKind::RoundPassed),
    ]);

    let report = JobReport::from_events(1, &events);

    assert_eq!(report.rounds, 2);
    assert_eq!(report.duration.unwrap().as_secs(), 3);
}

/// A torn line is skipped when the log is read, so a round's start can be
/// missing; the rounds after it keep their numbers.
#[test]
fn a_missing_round_start_does_not_lower_the_round_count() {
    let events = timeline(vec![
        (0, EventKind::RoundStarted { round: 1 }),
        (1, EventKind::RoundPassed),
        (2, EventKind::RoundStarted { round: 3 }),
    ]);

    assert_eq!(JobReport::from_events(1, &events).rounds, 3);
}

#[test]
fn a_round_started_twice_is_counted_once() {
    let events = timeline(vec![
        (0, EventKind::RoundStarted { round: 2 }),
        (1, EventKind::RoundStarted { round: 2 }),
    ]);

    assert_eq!(JobReport::from_events(1, &events).rounds, 2);
}

#[test]
fn a_new_round_clears_the_previous_rounds_failure_reason() {
    let events = timeline(vec![
        (0, EventKind::RoundStarted { round: 1 }),
        (
            1,
            EventKind::RoundFailed {
                reason: "exit 1".into(),
            },
        ),
        (2, EventKind::RoundStarted { round: 2 }),
        (3, EventKind::RoundPassed),
    ]);

    let report = JobReport::from_events(1, &events);

    assert_eq!(report.state, JobState::Passed);
    assert!(report.detail.is_none(), "stale reason survived");
    assert!(report.diff.is_none(), "stale diff survived");
}

/// `verify`'s ruling is not the round's verdict: the reason a rejected round
/// reports is the one its `RoundFailed` records.
#[test]
fn a_rejected_rounds_reason_comes_from_its_verdict_alone() {
    let ruled = timeline(vec![
        (0, EventKind::RoundStarted { round: 1 }),
        (
            1,
            EventKind::VerifyRejected {
                reason: "tests failed".into(),
            },
        ),
    ]);
    let report = JobReport::from_events(1, &ruled);
    assert_eq!(report.state, JobState::Running);
    assert!(report.detail.is_none(), "{:?}", report.detail);

    let failed = [
        ruled,
        timeline(vec![(
            2,
            EventKind::RoundFailed {
                reason: "verify rejected the work: tests failed".into(),
            },
        )]),
    ]
    .concat();
    assert_eq!(
        JobReport::from_events(1, &failed).detail.as_deref(),
        Some("verify rejected the work: tests failed")
    );
}

#[test]
fn a_job_reports_the_diff_it_committed() {
    let events = timeline(vec![
        (0, EventKind::RoundStarted { round: 1 }),
        (1, committed(3, 120, 4)),
        (2, EventKind::RoundPassed),
    ]);

    let diff = JobReport::from_events(1, &events).diff.expect("a diff");

    assert_eq!((diff.files, diff.insertions, diff.deletions), (3, 120, 4));
    assert_eq!(diff.to_string(), "3 files +120/-4");
}

#[test]
fn one_changed_file_is_not_pluralised() {
    let events = timeline(vec![(0, committed(1, 2, 0))]);
    let diff = JobReport::from_events(1, &events).diff.unwrap();

    assert_eq!(diff.to_string(), "1 file +2/-0");
}

/// A job's branch is its whole durable output, so the report has to name it —
/// including when the job failed, which is when it matters most.
#[test]
fn the_branch_is_reported_even_for_a_failed_job() {
    let events = timeline(vec![
        (0, EventKind::RoundStarted { round: 1 }),
        (1, committed(1, 1, 0)),
        (
            1,
            EventKind::BranchPushed {
                branch: "al/job-4".into(),
                pushed_to: "origin".into(),
            },
        ),
        (
            2,
            EventKind::RoundFailed {
                reason: "verify failed".into(),
            },
        ),
    ]);

    let report = JobReport::from_events(4, &events);

    assert_eq!(report.state, JobState::Failed);
    assert_eq!(report.branch.as_deref(), Some("al/job-4"));
}

#[test]
fn the_summary_line_names_the_round_the_diff_and_the_reason() {
    let events = timeline(vec![
        (0, EventKind::RoundStarted { round: 1 }),
        (0, EventKind::RoundPassed),
        (0, EventKind::RoundStarted { round: 2 }),
        (1, committed(3, 40, 2)),
        (
            2,
            EventKind::RoundFailed {
                reason: "verify failed".into(),
            },
        ),
    ]);

    assert_eq!(
        JobReport::from_events(7, &events).to_summary_line(),
        "job 7: failed (round 2, 3 files +40/-2) — verify failed"
    );
}

#[test]
fn the_summary_line_of_an_untouched_job_says_pending() {
    assert_eq!(
        JobReport::from_events(1, &[]).to_summary_line(),
        "job 1: pending (round 1)"
    );
}

#[test]
fn status_of_a_finished_job_names_its_timing_and_branch() {
    let events = timeline(vec![
        (0, EventKind::RoundStarted { round: 1 }),
        (1, committed(1, 1, 0)),
        (
            1,
            EventKind::BranchPushed {
                branch: "al/job-4".into(),
                pushed_to: "origin".into(),
            },
        ),
        (3, EventKind::RoundPassed),
    ]);

    assert_eq!(
        JobReport::from_events(4, &events).to_status_lines(),
        [
            "job 4: passed (round 1, 1 file +1/-0)",
            "took 3.0s",
            "branch: al/job-4",
        ]
    );
}

#[test]
fn status_of_an_untouched_job_is_its_summary_alone() {
    assert_eq!(
        JobReport::from_events(1, &[]).to_status_lines(),
        ["job 1: pending (round 1)"]
    );
}

#[test]
fn timing_is_reported_only_once_a_round_has_ended() {
    let running = timeline(vec![(0, EventKind::RoundStarted { round: 1 })]);
    assert!(
        JobReport::from_events(1, &running)
            .to_duration_line()
            .is_none()
    );

    let ended = timeline(vec![
        (0, EventKind::RoundStarted { round: 1 }),
        (2, EventKind::RoundPassed),
    ]);
    assert_eq!(
        JobReport::from_events(1, &ended)
            .to_duration_line()
            .unwrap(),
        "took 2.0s"
    );
}

#[test]
fn a_jobs_identity_is_its_first_requested_round() {
    let requested = |prompt: &str, sha: &str| EventKind::RoundRequested {
        remote_url: "git@github.com:o/r.git".into(),
        base: PinnedRef {
            name: "main".into(),
            sha: sha.into(),
        },
        prompt: prompt.into(),
        provider: "claude".into(),
    };
    let events = timeline(vec![
        (0, requested("add auth", "a1")),
        (1, requested("use sessions", "b2")),
    ]);

    let report = JobReport::from_events(1, &events);

    assert_eq!(report.remote_url.as_deref(), Some("git@github.com:o/r.git"));
    assert_eq!(report.first_prompt.as_deref(), Some("add auth"));
    assert_eq!(report.base.map(|base| base.sha).as_deref(), Some("b2"));
    assert_eq!(report.provider.as_deref(), Some("claude"));
}
