use assembly_line::event::{Event, EventKind};
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
    EventKind::JobCommitted {
        sha: "abc".into(),
        files,
        insertions,
        deletions,
    }
}

#[test]
fn summarizes_state_rounds_duration_and_detail() {
    let events = timeline(vec![
        (0, EventKind::JobStarted { round: 1 }),
        (
            9,
            EventKind::JobFailed {
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
        (0, EventKind::JobStarted { round: 1 }),
        (10, EventKind::JobFinished { exit_code: 0 }),
        (10, EventKind::JobStarted { round: 2 }),
        (13, EventKind::JobFinished { exit_code: 0 }),
    ]);

    let report = JobReport::from_events(1, &events);

    assert_eq!(report.rounds, 2);
    assert_eq!(report.duration.unwrap().as_secs(), 3);
}

#[test]
fn a_new_round_clears_the_previous_rounds_failure_reason() {
    let events = timeline(vec![
        (0, EventKind::JobStarted { round: 1 }),
        (
            1,
            EventKind::JobFailed {
                reason: "exit 1".into(),
            },
        ),
        (2, EventKind::JobStarted { round: 2 }),
        (3, EventKind::JobFinished { exit_code: 0 }),
    ]);

    let report = JobReport::from_events(1, &events);

    assert_eq!(report.state, JobState::Succeeded);
    assert!(report.detail.is_none(), "stale reason survived");
    assert!(report.diff.is_none(), "stale diff survived");
}

#[test]
fn a_job_reports_the_diff_it_committed() {
    let events = timeline(vec![
        (0, EventKind::JobStarted { round: 1 }),
        (1, committed(3, 120, 4)),
        (2, EventKind::JobFinished { exit_code: 0 }),
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
        (0, EventKind::JobStarted { round: 1 }),
        (1, committed(1, 1, 0)),
        (
            1,
            EventKind::JobBranchPublished {
                branch: "al/job-4".into(),
                pushed_to: Some("origin".into()),
            },
        ),
        (
            2,
            EventKind::JobFailed {
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
        (0, EventKind::JobStarted { round: 1 }),
        (0, EventKind::JobFinished { exit_code: 0 }),
        (0, EventKind::JobStarted { round: 2 }),
        (1, committed(3, 40, 2)),
        (
            2,
            EventKind::JobFailed {
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
fn timing_is_reported_only_once_a_round_has_ended() {
    let running = timeline(vec![(0, EventKind::JobStarted { round: 1 })]);
    assert!(
        JobReport::from_events(1, &running)
            .to_duration_line()
            .is_none()
    );

    let ended = timeline(vec![
        (0, EventKind::JobStarted { round: 1 }),
        (2, EventKind::JobFinished { exit_code: 0 }),
    ]);
    assert_eq!(
        JobReport::from_events(1, &ended)
            .to_duration_line()
            .unwrap(),
        "took 2.0s"
    );
}
