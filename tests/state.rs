use assembly_line::event::{Event, EventKind};
use assembly_line::state::JobState;

fn stream(kinds: Vec<EventKind>) -> Vec<Event> {
    let now = chrono::Utc::now();
    kinds
        .into_iter()
        .map(|kind| Event { at: now, kind })
        .collect()
}

#[test]
fn a_job_starts_pending() {
    assert_eq!(JobState::default(), JobState::Pending);
    assert_eq!(JobState::replay(&[]), JobState::Pending);
}

#[test]
fn starting_and_finishing_moves_a_job_through_running_to_succeeded() {
    let mut st = JobState::default();

    st.apply(&EventKind::JobStarted { round: 1 });
    assert_eq!(st, JobState::Running);

    st.apply(&EventKind::JobFinished { exit_code: 0 });
    assert_eq!(st, JobState::Succeeded);
}

#[test]
fn a_failed_job_is_recorded_as_failed() {
    let mut st = JobState::default();
    st.apply(&EventKind::JobFailed {
        reason: "exit 1".into(),
    });
    assert_eq!(st, JobState::Failed);
}

#[test]
fn a_commit_is_progress_not_completion() {
    let mut st = JobState::default();

    st.apply(&EventKind::JobStarted { round: 1 });
    st.apply(&EventKind::JobCommitted {
        sha: "abc".into(),
        files: 2,
        insertions: 10,
        deletions: 1,
    });
    assert_eq!(st, JobState::Running, "a commit is not completion");

    st.apply(&EventKind::JobFinished { exit_code: 0 });
    assert_eq!(st, JobState::Succeeded);
}

/// A job's branch is published before its success is judged, so publishing
/// must not decide the outcome either way.
#[test]
fn publishing_a_branch_does_not_decide_the_outcome() {
    let failed = JobState::replay(&stream(vec![
        EventKind::JobStarted { round: 1 },
        EventKind::JobBranchPublished {
            branch: "al/job-1".into(),
            pushed_to: None,
        },
        EventKind::JobFailed {
            reason: "exit 3".into(),
        },
    ]));

    assert_eq!(failed, JobState::Failed);
}

#[test]
fn a_revise_round_puts_a_finished_job_back_into_running() {
    let st = JobState::replay(&stream(vec![
        EventKind::JobStarted { round: 1 },
        EventKind::JobFinished { exit_code: 0 },
        EventKind::JobStarted { round: 2 },
    ]));

    assert_eq!(st, JobState::Running);
}

#[test]
fn replay_reconstructs_the_final_state_from_the_log_alone() {
    let st = JobState::replay(&stream(vec![
        EventKind::JobStarted { round: 1 },
        EventKind::JobCommitted {
            sha: "abc".into(),
            files: 1,
            insertions: 1,
            deletions: 0,
        },
        EventKind::JobBranchPublished {
            branch: "al/job-1".into(),
            pushed_to: Some("origin".into()),
        },
        EventKind::JobFinished { exit_code: 0 },
    ]));

    assert_eq!(st, JobState::Succeeded);
}

#[test]
fn every_state_has_a_label() {
    assert_eq!(JobState::Pending.label(), "pending");
    assert_eq!(JobState::Running.label(), "running");
    assert_eq!(JobState::Succeeded.label(), "succeeded");
    assert_eq!(JobState::Failed.label(), "failed");
}
