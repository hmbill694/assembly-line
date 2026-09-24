use assembly_line::config::REPO_CONFIG_PATH;
use assembly_line::event::EventKind;
use assembly_line::git::{self, commit_all, head_sha};
use assembly_line::payload;
use assembly_line::state::JobState;
use assembly_line::workspace::job_branch_name;
use support::{Harness, config_running, provider_block};

mod support;

/// The contract the whole milestone rests on: the checkout is scratch, the
/// branch is the durable artifact.
#[tokio::test]
async fn a_job_leaves_one_branch_carrying_its_work() {
    let h = Harness::new().await;

    let outcome = h.run_job("write a file").await;

    assert!(outcome.succeeded);
    assert_eq!(outcome.state, JobState::Succeeded);
    assert!(
        outcome.has(|e| matches!(e, EventKind::JobCommitted { .. })),
        "the agent's work should be committed"
    );
    assert!(
        !h.files_on_remote_branch(&job_branch_name(outcome.job_id))
            .await
            .is_empty(),
        "the branch is the job's whole durable output"
    );
    assert!(
        h.scratch_is_empty(),
        "the checkout is scratch and never survives"
    );
}

/// A job is cut from the ref it names, not from whatever happens to be
/// checked out when it starts.
#[tokio::test]
async fn a_job_is_cut_from_the_ref_it_names_not_from_head() {
    let h = Harness::new().await;
    let earlier = head_sha(&h.repo).await.unwrap();
    git::run_allowing_failure(&h.repo, &["tag", "start-here"])
        .await
        .unwrap();
    let pushed = git::run_allowing_failure(&h.repo, &["push", "origin", "start-here"])
        .await
        .unwrap();
    assert!(pushed.succeeded(), "{}", pushed.stderr);

    // The branch moves on after the ref the job will name — on the remote
    // too, so the job could only miss it by honouring the ref.
    std::fs::write(h.repo.join("later.txt"), "after\n").unwrap();
    commit_all(&h.repo, "later work").await.unwrap().unwrap();
    support::publish_main(&h.repo).await;
    assert_ne!(head_sha(&h.repo).await.unwrap(), earlier);

    let outcome = h.run_job_from("go", "start-here").await;

    let branch = job_branch_name(outcome.job_id);
    let parent = git::run_allowing_failure(&h.origin, &["rev-parse", &format!("{branch}^")])
        .await
        .unwrap();
    assert_eq!(
        parent.stdout.trim(),
        earlier,
        "the job ignored the ref it was given"
    );

    let listed = h.files_on_remote_branch(&branch).await;
    assert!(
        !listed.contains("later.txt"),
        "the job saw commits the ref it named does not carry: {listed}"
    );
}

/// The security property the whole read-from-a-ref design exists for: a
/// checkout can say anything, and the settings that govern a job are not its
/// to rewrite.
#[tokio::test]
async fn a_job_obeys_the_committed_config_not_the_working_trees() {
    // Committed: the agent that writes `agent-output.txt` and exits 0.
    let h = Harness::new().await;

    // Uncommitted: a different provider entirely, which writes `partial.txt`
    // and fails. If the working tree governed, the job would fail.
    std::fs::write(
        h.repo.join(REPO_CONFIG_PATH),
        config_running("failing-agent.sh"),
    )
    .unwrap();

    let outcome = h.run_job("go").await;

    assert!(
        outcome.succeeded,
        "the working tree's provider ran: {:?}",
        outcome.events
    );
    let listed = h
        .files_on_remote_branch(&job_branch_name(outcome.job_id))
        .await;
    assert!(
        listed.contains("agent-output.txt"),
        "the committed provider did not run: {listed}"
    );
    assert!(
        !listed.contains("partial.txt"),
        "the working tree's config governed the job: {listed}"
    );
}

#[tokio::test]
async fn a_job_leaves_the_target_repositorys_working_tree_untouched() {
    let h = Harness::new().await;
    let base = head_sha(&h.repo).await.unwrap();

    let outcome = h.run_job("add authentication").await;

    assert!(outcome.succeeded);
    assert_eq!(head_sha(&h.repo).await.unwrap(), base);
    assert_eq!(
        git::current_branch(&h.repo).await.unwrap().as_deref(),
        Some("main")
    );
    assert!(!h.repo.join("agent-output.txt").exists());
    assert!(!git::is_dirty(&h.repo).await.unwrap());

    let listed = h
        .files_on_remote_branch(&job_branch_name(outcome.job_id))
        .await;
    assert!(listed.contains("agent-output.txt"), "{listed}");
}

#[tokio::test]
async fn the_prompt_reaches_the_agent_intact() {
    let h = Harness::new().await;

    let prompt = "quotes \" and $HOME and ; semicolons";
    let outcome = h.run_job(prompt).await;

    let content = h
        .file_on_remote_branch(&job_branch_name(outcome.job_id), "agent-output.txt")
        .await
        .expect("the agent's output reached the branch");
    // Equality, not `contains`: the quote is a hazard too, and a `contains`
    // pair would pass with it stripped.
    assert_eq!(content.trim_end_matches('\n'), prompt);
}

/// A half-finished failure is exactly the case where the diff is worth
/// reading, so the work is committed before the failure is judged.
#[tokio::test]
async fn a_failing_agent_preserves_its_work_on_a_branch_and_leaves_no_checkout() {
    let h = Harness::with_config(&config_running("failing-agent.sh")).await;

    let outcome = h.run_job("x").await;

    assert!(!outcome.succeeded);
    assert_eq!(outcome.state, JobState::Failed);
    assert!(
        outcome.has(|k| matches!(k, EventKind::JobFailed { reason } if reason.contains("exit 3")))
    );
    assert!(outcome.has(|k| matches!(k, EventKind::JobCommitted { .. })));

    let branch = job_branch_name(outcome.job_id);
    assert!(
        outcome
            .has(|k| matches!(k, EventKind::JobBranchPublished { branch: b, pushed_to } if *b == branch && pushed_to.as_deref() == Some("origin")))
    );
    assert!(
        h.scratch_is_empty(),
        "a job's checkout is scratch — even a failed one discards it"
    );

    // The work is on the branch, which is what makes the failure inspectable
    // from anywhere rather than only on the machine that ran it.
    let on_branch = h.files_on_remote_branch(&branch).await;
    assert!(
        on_branch.contains("partial.txt"),
        "the agent's partial work is not on the branch: {on_branch}"
    );
}

/// A job whose branch cannot leave the scratch clone has lost its work, and
/// must say so rather than record a branch that exists nowhere.
#[tokio::test]
async fn a_refused_push_fails_the_job_and_says_the_work_is_lost() {
    let h = Harness::new().await;

    // The remote already carries an unrelated `al/job-1`, so the job's push is
    // a non-fast-forward and git refuses it.
    let base = head_sha(&h.repo).await.unwrap();
    let tree = git::run_allowing_failure(&h.repo, &["rev-parse", &format!("{base}^{{tree}}")])
        .await
        .unwrap()
        .stdout
        .trim()
        .to_string();
    let unrelated = git::run_allowing_failure(&h.repo, &["commit-tree", &tree, "-m", "theirs"])
        .await
        .unwrap()
        .stdout
        .trim()
        .to_string();
    let pushed = git::run_allowing_failure(
        &h.repo,
        &[
            "push",
            &h.origin.to_string_lossy(),
            &format!("{unrelated}:refs/heads/al/job-1"),
        ],
    )
    .await
    .unwrap();
    assert!(pushed.succeeded(), "{}", pushed.stderr);

    let outcome = h.run_job("write a file").await;

    assert!(!outcome.succeeded);
    assert!(
        !outcome.has(|k| matches!(k, EventKind::JobBranchPublished { .. })),
        "a branch that never left the clone was recorded as published: {:?}",
        outcome.events
    );
    assert!(
        outcome.has(
            |k| matches!(k, EventKind::JobFailed { reason } if reason.contains("work is lost"))
        )
    );
    assert!(
        h.scratch_is_empty(),
        "the checkout leaked when the push failed"
    );

    // Without this the test would pass for the wrong reason: it only says
    // anything if the push was actually refused.
    let on_remote = git::run_allowing_failure(&h.origin, &["rev-parse", "al/job-1"])
        .await
        .unwrap();
    assert_eq!(
        on_remote.stdout.trim(),
        unrelated,
        "the push was not refused, so this test proves nothing"
    );
}

#[tokio::test]
async fn a_repository_with_no_remote_has_nothing_to_clone() {
    let h = Harness::new().await;
    git::run_allowing_failure(&h.repo, &["remote", "remove", "origin"])
        .await
        .unwrap();

    let err = payload::remote_to_clone(&h.repo, "origin")
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("no 'origin' remote"), "{err}");
}

#[tokio::test]
async fn an_agent_that_changes_nothing_succeeds_without_committing() {
    let h = Harness::with_config(&config_running("noop-agent.sh")).await;

    let outcome = h.run_job("x").await;

    assert!(outcome.succeeded);
    assert_eq!(outcome.state, JobState::Succeeded);
    assert!(!outcome.has(|k| matches!(k, EventKind::JobCommitted { .. })));
}

#[tokio::test]
async fn seeded_files_reach_the_agent_but_never_the_branch() {
    let h = Harness::with_config(&format!(
        "provider = \"fake\"\ncopy = [\".env\"]\n{}",
        provider_block("fake-agent.sh", "a")
    ))
    .await;
    std::fs::write(h.repo.join(".env"), "API_KEY=hunter2\n").unwrap();

    let outcome = h.run_job("x").await;
    assert!(outcome.succeeded);

    let listed = h
        .files_on_remote_branch(&job_branch_name(outcome.job_id))
        .await;
    assert!(
        !listed.contains(".env"),
        "the seeded secret reached a branch: {listed}"
    );
}

#[tokio::test]
async fn a_missing_provider_binary_fails_the_job_with_a_useful_message() {
    let h = Harness::with_config(
        "provider = \"fake\"\n\
         [providers.fake]\ncmd = \"definitely-not-real-xyz\"\nargs = [\"{prompt}\"]\n",
    )
    .await;

    let outcome = h.run_job("x").await;

    assert!(!outcome.succeeded);
    assert!(
        outcome.has(
            |k| matches!(k, EventKind::JobFailed { reason } if reason.contains("definitely-not-real-xyz"))
        ),
        "{:?}",
        outcome.events
    );
}

// The line between a job that fails and a job that cannot be administered at
// all. The first is an ordinary outcome recorded in the event log; the second
// never starts, and is reported against the command line instead.

#[tokio::test]
async fn a_provider_the_repository_never_declared_stops_the_job_before_it_starts() {
    let h = Harness::new().await;

    let err = h
        .attempt_round("x", Some("ghost"), None, 1)
        .await
        .expect_err("an undeclared provider is not a job that failed")
        .to_string();

    assert!(err.contains("ghost"), "{err}");
    assert!(err.contains("add a block for it"), "{err}");
}

#[tokio::test]
async fn an_unparseable_max_duration_stops_the_job_before_it_starts() {
    let h = Harness::with_config(&format!(
        "provider = \"fake\"\nmax_duration = \"soon\"\n{}",
        provider_block("fake-agent.sh", "a")
    ))
    .await;

    let err = h
        .attempt_round("x", None, None, 1)
        .await
        .expect_err("an unparseable cap is not a job that failed")
        .to_string();

    assert!(err.contains("soon"), "{err}");
}

/// The heart of the stateless design: a revise round is a new job that sees
/// its prior work because that work *is* the branch it starts from. Nothing
/// was kept on disk between the rounds.
#[tokio::test]
async fn a_revise_round_continues_the_branch_instead_of_starting_over() {
    let h = Harness::with_config(&config_running("revising-agent.sh")).await;

    let first = h.run_job("hi").await;
    assert!(first.succeeded);

    let second = h.revise_job("add error handling", 2).await;
    assert!(second.succeeded);

    let branch = job_branch_name(second.job_id);
    let body = h
        .file_on_remote_branch(&branch, "rounds.txt")
        .await
        .unwrap_or_default();
    assert!(
        body.starts_with("hi\n"),
        "round 1's line is gone, so round 2 started from scratch: {body}"
    );
    assert!(
        body.contains("add error handling"),
        "round 2 never saw the feedback: {body}"
    );
}
