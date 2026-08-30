use assembly_line::event::{Event, EventKind};
use assembly_line::review::{ReviewInbox, ReviewState};
use chrono::{TimeZone, Utc};

/// Events carry a timestamp the inbox never reads, so a fixed one keeps these
/// tests about the fold and nothing else.
fn at(kind: EventKind) -> Event {
    Event {
        at: Utc.with_ymd_and_hms(2026, 8, 30, 12, 0, 0).unwrap(),
        kind,
    }
}

fn node(name: &str) -> String {
    name.to_string()
}

/// The events a node emits when it runs, commits, publishes and merges — the
/// ordinary happy path a gate is then layered onto.
fn landed(run_id: u64, name: &str, files: usize) -> Vec<Event> {
    vec![
        at(EventKind::NodeStarted {
            node: node(name),
            round: 1,
        }),
        at(EventKind::NodeCommitted {
            node: node(name),
            sha: "abc123".into(),
            files,
            insertions: 10,
            deletions: 2,
        }),
        at(EventKind::NodeBranchPublished {
            node: node(name),
            branch: format!("al/run-{run_id}-{name}"),
            pushed_to: None,
        }),
        at(EventKind::NodeMerged {
            node: node(name),
            sha: "def456".into(),
        }),
        at(EventKind::NodeFinished {
            node: node(name),
            exit_code: 0,
        }),
    ]
}

#[test]
fn a_node_with_no_gate_is_not_in_the_inbox() {
    let events = landed(1, "work", 1);
    let inbox = ReviewInbox::from_events(1, &events);

    assert_eq!(inbox.item("work").unwrap().state, ReviewState::NotGated);
    assert!(inbox.awaiting_review().is_empty());
}

#[test]
fn a_deferred_gate_puts_the_node_in_the_inbox_with_its_branch_and_diff() {
    let mut events = landed(42, "impl-auth", 3);
    events.push(at(EventKind::NodeAwaitingReview {
        node: node("impl-auth"),
    }));

    let inbox = ReviewInbox::from_events(42, &events);
    let waiting = inbox.awaiting_review();

    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].node, "impl-auth");
    assert_eq!(
        waiting[0].branch.as_deref(),
        Some("al/run-42-impl-auth"),
        "a reviewer needs the branch to read"
    );
    assert_eq!(waiting[0].diff.unwrap().files, 3);
}

#[test]
fn approving_takes_a_node_out_of_the_inbox() {
    let mut events = landed(1, "impl-auth", 1);
    events.push(at(EventKind::NodeAwaitingReview {
        node: node("impl-auth"),
    }));
    events.push(at(EventKind::NodeApproved {
        node: node("impl-auth"),
    }));

    let inbox = ReviewInbox::from_events(1, &events);

    assert_eq!(
        inbox.item("impl-auth").unwrap().state,
        ReviewState::Approved
    );
    assert!(inbox.awaiting_review().is_empty());
}

#[test]
fn requesting_a_revision_records_the_feedback() {
    let mut events = landed(1, "impl-auth", 1);
    events.push(at(EventKind::NodeAwaitingReview {
        node: node("impl-auth"),
    }));
    events.push(at(EventKind::NodeRevisionRequested {
        node: node("impl-auth"),
        feedback: "use argon2, not bcrypt".into(),
    }));

    let inbox = ReviewInbox::from_events(1, &events);
    let item = inbox.item("impl-auth").unwrap();

    assert_eq!(item.state, ReviewState::RevisionRequested);
    assert_eq!(item.feedback.as_deref(), Some("use argon2, not bcrypt"));
    // Sent back is not waiting: the next round is what gets looked at.
    assert!(inbox.awaiting_review().is_empty());
}

/// A revise round is a new job. Its verdict supersedes the last one, so the
/// node leaves the inbox until the new round lands and re-enters it.
#[test]
fn a_new_round_clears_the_previous_verdict() {
    let mut events = landed(1, "impl-auth", 1);
    events.push(at(EventKind::NodeAwaitingReview {
        node: node("impl-auth"),
    }));
    events.push(at(EventKind::NodeRevisionRequested {
        node: node("impl-auth"),
        feedback: "again".into(),
    }));
    events.push(at(EventKind::NodeStarted {
        node: node("impl-auth"),
        round: 2,
    }));

    let inbox = ReviewInbox::from_events(1, &events);
    let item = inbox.item("impl-auth").unwrap();

    assert_eq!(item.state, ReviewState::NotGated, "round 2 is in flight");
    assert_eq!(
        item.feedback.as_deref(),
        Some("again"),
        "the feedback the round is acting on is still worth showing"
    );
}

#[test]
fn nodes_appear_in_the_order_they_ran() {
    let events: Vec<Event> = ["zebra", "apple", "mango"]
        .iter()
        .flat_map(|name| {
            let mut evs = landed(1, name, 1);
            evs.push(at(EventKind::NodeAwaitingReview { node: node(name) }));
            evs
        })
        .collect();

    let inbox = ReviewInbox::from_events(1, &events);
    let order: Vec<&str> = inbox.items.iter().map(|i| i.node.as_str()).collect();

    assert_eq!(order, vec!["zebra", "apple", "mango"]);
}

#[test]
fn an_empty_inbox_says_so_rather_than_printing_a_bare_header() {
    let inbox = ReviewInbox::from_events(7, &landed(7, "work", 1));
    assert!(inbox.to_terminal_list().contains("nothing awaiting review"));
}

#[test]
fn the_listing_shows_what_a_reviewer_needs_and_how_to_act() {
    let mut events = landed(42, "impl-auth", 3);
    events.push(at(EventKind::NodeAwaitingReview {
        node: node("impl-auth"),
    }));

    let listing = ReviewInbox::from_events(42, &events).to_terminal_list();

    assert!(listing.contains("1 awaiting review"), "{listing}");
    assert!(listing.contains("impl-auth"), "{listing}");
    assert!(listing.contains("al/run-42-impl-auth"), "{listing}");
    assert!(listing.contains("3 files +10/-2"), "{listing}");
    assert!(listing.contains("--approve"), "{listing}");
    assert!(listing.contains("--revise"), "{listing}");
}
