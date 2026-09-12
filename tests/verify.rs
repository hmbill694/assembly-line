mod support;

use assembly_line::event::EventKind;
use assembly_line::git;
use assembly_line::workspace::job_branch_name;
use support::{Harness, config_running};

/// A `verify` line that can only pass in the job's own checkout, *after* the
/// commit: it asks whether the file the fake agent wrote is in `HEAD`.
///
/// `exit 0` would pass wherever and whenever it ran, which is what makes it
/// useless for pinning the ordering the whole task rests on.
const VERIFY_SEES_THE_COMMITTED_WORK: &str =
    "git show --name-only --format= HEAD | grep -q agent-output.txt";

/// The negation, for the rejecting case: it fails only where and when the
/// work is actually there, so a `verify` run in the wrong place or before the
/// commit would *accept* and the test would notice.
const VERIFY_REJECTS_THE_COMMITTED_WORK: &str =
    "! git show --name-only --format= HEAD | grep -q agent-output.txt";

/// What a `verify` prints so a test can tell it ran *at all*.
///
/// The event log cannot answer that question: a verify whose answer is thrown
/// away writes no event, exactly like a verify that was never started. Its
/// output, though, goes to the same job log the agent writes to, so the
/// presence of this line is direct evidence of execution.
const VERIFY_RAN: &str = "verify-ran-in-this-checkout";

/// The invariant the whole milestone rests on, exercised through the one path
/// this task adds: a rejection is a judgement about delivery, not a reason to
/// take the branch away. The branch reaches the remote, and the checkout is
/// still scratch and still discarded.
#[tokio::test]
async fn a_job_whose_verify_fails_is_a_failed_job() {
    let h = Harness::with_config(&format!(
        "verify = \"{VERIFY_REJECTS_THE_COMMITTED_WORK}\"\n{}",
        config_running("fake-agent.sh")
    ))
    .await;
    let origin = h.with_origin().await;

    let outcome = h.run_job("write a file").await;

    assert!(!outcome.succeeded, "a failing verify fails the job");
    assert!(
        outcome.has(|e| matches!(e, EventKind::JobVerifyFailed { .. })),
        "the failure should say verify was what rejected it"
    );
    assert!(
        outcome.has(|e| matches!(e, EventKind::JobCommitted { .. })),
        "the agent's work is still committed even though verify rejected it"
    );

    let branch = job_branch_name(outcome.job_id);
    assert!(
        outcome.has(|e| matches!(
            e,
            EventKind::JobBranchPublished { branch: b, pushed_to }
                if *b == branch && pushed_to.as_deref() == Some("origin")
        )),
        "the branch survives a failed verify — that is what makes it inspectable"
    );
    assert!(
        !h.worktree_root().join("checkout").exists(),
        "a job's checkout is scratch — even one verify rejected discards it"
    );

    let on_remote =
        git::run_allowing_failure(&origin, &["show", "--name-only", "--format=", &branch])
            .await
            .unwrap();
    assert!(on_remote.succeeded(), "{}", on_remote.stderr);
    assert!(
        on_remote.stdout.contains("agent-output.txt"),
        "the rejected work never reached the remote: {}",
        on_remote.stdout
    );
}

/// Also the positive control for `an_agent_failure_wins_over_verify`: the
/// same sentinel that must be absent there must be present here, or that test
/// would pass for a verify that never runs under any circumstances.
#[tokio::test]
async fn a_job_whose_verify_passes_succeeds() {
    let h = Harness::with_config(&format!(
        "verify = \"echo {VERIFY_RAN} && {VERIFY_SEES_THE_COMMITTED_WORK}\"\n{}",
        config_running("fake-agent.sh")
    ))
    .await;

    let outcome = h.run_job("write a file").await;

    assert!(
        outcome.succeeded,
        "verify runs in the job's checkout after the commit, so it sees the work"
    );
    assert!(!outcome.has(|e| matches!(e, EventKind::JobVerifyFailed { .. })));
    assert!(
        std::fs::read_to_string(&outcome.log)
            .unwrap()
            .contains(VERIFY_RAN),
        "a round the agent succeeded is a round verify is asked about"
    );
}

#[tokio::test]
async fn a_job_with_no_verify_succeeds_on_the_agents_exit_code() {
    // No `verify` key at all — not an empty config, which would declare no
    // provider either and leave the job with nothing to run.
    let h = Harness::with_config(&config_running("fake-agent.sh")).await;

    let outcome = h.run_job("write a file").await;

    assert!(outcome.succeeded);
}

/// The composition rule: an agent failure is the earlier and more fundamental
/// one, so it wins outright — and verify is never *asked*, which is a
/// stronger claim than merely being out-voted.
///
/// The event assertions alone cannot tell those apart: a verify whose answer
/// is discarded also writes no `JobVerifyFailed`. The job log can, which is
/// why this `verify` announces itself before failing.
#[tokio::test]
async fn an_agent_failure_wins_over_verify() {
    let h = Harness::with_config(&format!(
        "verify = \"echo {VERIFY_RAN}; exit 1\"\n{}",
        config_running("failing-agent.sh")
    ))
    .await;

    let outcome = h.run_job("write a file").await;

    assert!(!outcome.succeeded);
    assert!(
        outcome.has(|e| matches!(e, EventKind::JobFailed { reason } if reason == "exit 3")),
        "the job failed for the agent's reason, not verify's: {:?}",
        outcome.events
    );
    assert!(
        !outcome.has(|e| matches!(e, EventKind::JobVerifyFailed { .. })),
        "nothing may claim verify rejected work it never looked at"
    );
    let job_log = std::fs::read_to_string(&outcome.log).unwrap();
    assert!(
        !job_log.contains(VERIFY_RAN),
        "verify ran on a round the agent had already failed: {job_log}"
    );
    assert!(
        outcome.has(|e| matches!(e, EventKind::JobCommitted { .. })),
        "the work the agent got as far as is still committed"
    );
}

/// A verify that was killed judged nothing. Recording that as a rejection
/// would write a false claim about the work into a log that is append-only
/// and can never be corrected.
#[tokio::test]
async fn a_verify_cut_off_by_max_duration_is_not_a_rejection() {
    let h = Harness::with_config(&format!(
        "verify = \"sleep 30\"\nmax_duration = \"1s\"\n{}",
        config_running("fake-agent.sh")
    ))
    .await;

    let outcome = h.run_job("write a file").await;

    assert!(!outcome.succeeded, "an unfinished verify still fails a job");
    assert!(
        !outcome.has(|e| matches!(e, EventKind::JobVerifyFailed { .. })),
        "a timed-out verify reached no verdict, so it is not a rejection"
    );
    assert!(
        outcome
            .has(|e| matches!(e, EventKind::JobFailed { reason } if reason == "verify timed out")),
        "the failure names what actually happened: {:?}",
        outcome.events
    );
}
