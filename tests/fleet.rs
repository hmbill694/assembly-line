//! What a daemon starting on a root owes each job it finds there.

use assembly_line::daemon::fleet::{Resumption, jobs_under};
use assembly_line::event::{Event, EventKind, EventLog};
use assembly_line::git::PinnedRef;
use assembly_line::job::JobId;
use assembly_line::paths::JobPlace;
use assembly_line::remote::Remote;
use assembly_line::report::JobReport;
use assembly_line::runner::RoundHandle;
use std::path::Path;

fn report_of(kinds: Vec<EventKind>) -> JobReport {
    let events: Vec<Event> = kinds
        .into_iter()
        .map(|kind| Event {
            at: chrono::Utc::now(),
            kind,
        })
        .collect();
    JobReport::from_events(JobId::from(1), &events)
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

fn requested_job(root: &Path, url: &str, id: u64) -> JobPlace {
    let place = JobPlace {
        key: Remote::from_url(url).repo_key().unwrap(),
        id: JobId::from(id),
    };
    place.create(root).unwrap().append(requested()).unwrap();
    place
}

/// Every job under the root is found at the place its directories name, and
/// what is not a job's — a directory without a log, a name that is not an
/// id, a repository directory no key is named by — is passed over.
#[test]
fn every_job_under_the_root_is_found_by_its_place() {
    let root = tempfile::tempdir().unwrap();
    let places = [1, 2].map(|id| requested_job(root.path(), "/o.git", id));
    let jobs_dir = places[0].key.jobs_dir(root.path());
    std::fs::create_dir_all(jobs_dir.join("3")).unwrap();
    [jobs_dir.join("notes"), root.path().join("jobs/not a key/4")]
        .iter()
        .for_each(|dir| {
            EventLog::open_append(dir.join("events.jsonl"))
                .unwrap()
                .append(requested())
                .unwrap();
        });

    let mut found: Vec<(JobPlace, JobId)> = jobs_under(root.path())
        .unwrap()
        .into_iter()
        .map(|(place, report)| (place, report.id))
        .collect();
    found.sort_by_key(|(place, _)| place.id);

    assert_eq!(
        found,
        places.map(|place| {
            let id = place.id;
            (place, id)
        })
    );
}

/// `/x/a/7.git`'s jobs directory and `/x/a.git`'s job 7 used to be one
/// directory, and the walk stopped at the first log it found.
#[test]
fn jobs_of_repositories_whose_directories_once_nested_are_both_resumed() {
    let root = tempfile::tempdir().unwrap();
    let places =
        [("/x/a/7.git", 1), ("/x/a.git", 7)].map(|(url, id)| requested_job(root.path(), url, id));

    let mut found: Vec<(JobPlace, Option<Resumption>)> = jobs_under(root.path())
        .unwrap()
        .into_iter()
        .map(|(place, report)| (place, Resumption::for_report(&report)))
        .collect();
    found.sort_by_key(|(place, _)| place.id);

    assert_eq!(
        found,
        places.map(|place| (place, Some(Resumption::Requeue)))
    );
}

#[test]
fn a_root_with_no_jobs_yet_owes_nothing() {
    let root = tempfile::tempdir().unwrap();

    assert!(jobs_under(root.path()).unwrap().is_empty());
}
