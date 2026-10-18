//! What a daemon starting on a root owes each job it finds there.

use assembly_line::daemon::fleet::{Resumption, jobs_under};
use assembly_line::event::{Event, EventKind, EventLog};
use assembly_line::git::PinnedRef;
use assembly_line::paths::RepoKey;
use assembly_line::report::JobReport;
use assembly_line::runner::RoundHandle;

fn report_of(kinds: Vec<EventKind>) -> JobReport {
    let events: Vec<Event> = kinds
        .into_iter()
        .map(|kind| Event {
            at: chrono::Utc::now(),
            kind,
        })
        .collect();
    JobReport::from_events(1, &events)
}

fn requested() -> EventKind {
    EventKind::RoundRequested {
        remote_url: "/o.git".into(),
        base: PinnedRef {
            name: "main".into(),
            sha: "a".repeat(40),
        },
        prompt: "x".into(),
        provider: "p".into(),
    }
}

fn container() -> RoundHandle {
    RoundHandle::Docker {
        container: "al-1-1-x".into(),
    }
}

#[test]
fn a_job_still_queued_goes_back_in_the_queue() {
    assert_eq!(
        Resumption::for_report(&report_of(vec![requested()])),
        Some(Resumption::Requeue)
    );
}

#[test]
fn a_round_launched_without_a_verdict_is_reattached() {
    let report = report_of(vec![
        requested(),
        EventKind::RoundStarted { round: 1 },
        EventKind::RoundLaunched {
            handle: container(),
        },
    ]);

    assert_eq!(
        Resumption::for_report(&report),
        Some(Resumption::Reattach {
            round: 1,
            handle: container()
        })
    );
}

#[test]
fn a_round_started_but_never_launched_was_lost_while_launching() {
    let report = report_of(vec![requested(), EventKind::RoundStarted { round: 2 }]);

    assert_eq!(
        Resumption::for_report(&report),
        Some(Resumption::LostWhileLaunching { round: 2 })
    );
}

/// A revise's round is not found by the handle its previous round recorded.
#[test]
fn a_later_round_never_inherits_an_earlier_rounds_handle() {
    let report = report_of(vec![
        requested(),
        EventKind::RoundStarted { round: 1 },
        EventKind::RoundLaunched {
            handle: container(),
        },
        EventKind::RoundPassed,
        requested(),
        EventKind::RoundStarted { round: 2 },
    ]);

    assert_eq!(
        Resumption::for_report(&report),
        Some(Resumption::LostWhileLaunching { round: 2 })
    );
}

#[test]
fn a_job_with_a_verdict_is_owed_nothing() {
    let report = report_of(vec![
        requested(),
        EventKind::RoundStarted { round: 1 },
        EventKind::RoundLaunched {
            handle: container(),
        },
        EventKind::RoundPassed,
    ]);

    assert_eq!(Resumption::for_report(&report), None);
}

#[test]
fn a_handle_round_trips_through_the_event_log() {
    let handle = RoundHandle::Local {
        pid: 42,
        frames: "/r/round-1.frames".into(),
    };
    let line = serde_json::to_string(&EventKind::RoundLaunched {
        handle: handle.clone(),
    })
    .unwrap();

    assert!(line.contains("\"runner\":\"local\""), "{line}");
    assert_eq!(
        serde_json::from_str::<EventKind>(&line).unwrap(),
        EventKind::RoundLaunched { handle }
    );
}

/// Every job under the root is found where its remote's key puts it, and
/// what is not a job's — a directory without a log, a name that is not an
/// id — is passed over.
#[test]
fn every_job_under_the_root_is_found_by_its_address() {
    let root = tempfile::tempdir().unwrap();
    let key = RepoKey::from_remote_url("/o.git").unwrap();
    let jobs_dir = key.jobs_dir(root.path());
    [1, 2].iter().for_each(|id| {
        EventLog::open_append(jobs_dir.join(id.to_string()).join("events.jsonl"))
            .unwrap()
            .append(requested())
            .unwrap();
    });
    std::fs::create_dir_all(jobs_dir.join("3")).unwrap();
    EventLog::open_append(jobs_dir.join("notes").join("events.jsonl"))
        .unwrap()
        .append(requested())
        .unwrap();

    let mut found: Vec<(u64, RepoKey, std::path::PathBuf)> = jobs_under(root.path())
        .unwrap()
        .into_iter()
        .map(|(address, report)| (report.id, address.key, address.jobs_dir))
        .collect();
    found.sort_by_key(|(id, ..)| *id);

    assert_eq!(
        found,
        [(1, key.clone(), jobs_dir.clone()), (2, key, jobs_dir)]
    );
}

#[test]
fn a_root_with_no_jobs_yet_owes_nothing() {
    let root = tempfile::tempdir().unwrap();

    assert!(jobs_under(root.path()).unwrap().is_empty());
}
