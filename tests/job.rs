use assembly_line::config::REPO_CONFIG_PATH;
use assembly_line::event::EventKind;
use assembly_line::git::{self, commit_all, head_sha};
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
        git::branch_exists(&h.repo, &job_branch_name(outcome.job_id))
            .await
            .unwrap(),
        "the branch is the job's whole durable output"
    );
    assert!(
        !h.worktree_root().join("checkout").exists(),
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

    // The branch moves on after the ref the job will name.
    std::fs::write(h.repo.join("later.txt"), "after\n").unwrap();
    commit_all(&h.repo, "later work").await.unwrap().unwrap();
    assert_ne!(head_sha(&h.repo).await.unwrap(), earlier);

    let outcome = h.run_job_from("go", "start-here").await;

    let branch = job_branch_name(outcome.job_id);
    let parent = git::run_allowing_failure(&h.repo, &["rev-parse", &format!("{branch}^")])
        .await
        .unwrap();
    assert_eq!(
        parent.stdout.trim(),
        earlier,
        "the job ignored the ref it was given"
    );

    let listed = git::run_allowing_failure(&h.repo, &["ls-tree", "--name-only", "-r", &branch])
        .await
        .unwrap()
        .stdout;
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
    let listed = git::run_allowing_failure(
        &h.repo,
        &[
            "ls-tree",
            "--name-only",
            "-r",
            &job_branch_name(outcome.job_id),
        ],
    )
    .await
    .unwrap()
    .stdout;
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

    let listed = git::run_allowing_failure(
        &h.repo,
        &["ls-tree", "--name-only", &job_branch_name(outcome.job_id)],
    )
    .await
    .unwrap()
    .stdout;
    assert!(listed.contains("agent-output.txt"), "{listed}");
}

#[tokio::test]
async fn the_prompt_reaches_the_agent_intact() {
    let h = Harness::new().await;

    let prompt = "quotes \" and $HOME and ; semicolons";
    let outcome = h.run_job(prompt).await;

    let content = git::run_allowing_failure(
        &h.repo,
        &[
            "show",
            &format!("{}:agent-output.txt", job_branch_name(outcome.job_id)),
        ],
    )
    .await
    .unwrap()
    .stdout;
    // Equality, not `contains`: the quote is a hazard too, and a `contains`
    // pair would pass with it stripped.
    assert_eq!(content.trim_end_matches('\n'), prompt);
}

/// A half-finished failure is exactly the case where the diff is worth
/// reading, so the work is committed before the failure is judged.
#[tokio::test]
async fn a_failing_agent_preserves_its_work_on_a_branch_and_leaves_no_worktree() {
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
            .has(|k| matches!(k, EventKind::JobBranchPublished { branch: b, .. } if *b == branch))
    );
    assert!(
        !h.worktree_root().join("checkout").exists(),
        "a job's checkout is scratch — even a failed one discards it"
    );

    // The work is on the branch, which is what makes the failure inspectable
    // from anywhere rather than only on the machine that ran it.
    let on_branch =
        git::run_allowing_failure(&h.repo, &["show", "--name-only", "--format=", &branch])
            .await
            .unwrap();
    assert!(on_branch.succeeded(), "{}", on_branch.stderr);
    assert!(
        on_branch.stdout.contains("partial.txt"),
        "the agent's partial work is not on the branch: {}",
        on_branch.stdout
    );
}

/// The point of publishing: a failed job's work leaves the machine that ran it.
#[tokio::test]
async fn a_failed_jobs_branch_reaches_the_remote() {
    let h = Harness::with_config(&config_running("failing-agent.sh")).await;
    let origin = h.with_origin().await;

    let outcome = h.run_job("x").await;

    assert!(!outcome.succeeded);
    assert!(outcome.has(
        |k| matches!(k, EventKind::JobBranchPublished { pushed_to, .. } if pushed_to.as_deref() == Some("origin"))
    ));

    let on_remote = git::run_allowing_failure(
        &origin,
        &[
            "show",
            "--name-only",
            "--format=",
            &job_branch_name(outcome.job_id),
        ],
    )
    .await
    .unwrap();
    assert!(on_remote.succeeded(), "{}", on_remote.stderr);
    assert!(
        on_remote.stdout.contains("partial.txt"),
        "the failed job's work never reached the remote: {}",
        on_remote.stdout
    );
}

/// A remote that refuses the push must not cost the job the record of its own
/// branch. The branch exists locally and holds the work — which is the whole
/// durable artifact — so it is still recorded, with `pushed_to: None`, and the
/// scratch checkout still goes.
#[tokio::test]
async fn a_refused_push_still_records_the_branch_and_still_discards_the_checkout() {
    let h = Harness::new().await;
    let origin = h.with_origin().await;

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
            &origin.to_string_lossy(),
            &format!("{unrelated}:refs/heads/al/job-1"),
        ],
    )
    .await
    .unwrap();
    assert!(pushed.succeeded(), "{}", pushed.stderr);

    let outcome = h.run_job("write a file").await;

    assert!(
        outcome.succeeded,
        "a remote refusing the push is not the agent's failure: {:?}",
        outcome.events
    );
    assert!(
        outcome.has(
            |k| matches!(k, EventKind::JobBranchPublished { pushed_to, .. } if pushed_to.is_none())
        ),
        "the branch fell out of the record when the push was refused: {:?}",
        outcome.events
    );
    assert!(
        git::branch_exists(&h.repo, &job_branch_name(outcome.job_id))
            .await
            .unwrap(),
        "the local branch is the durable artifact and must survive"
    );
    assert!(
        !h.worktree_root().join("checkout").exists(),
        "the checkout leaked when the push failed"
    );

    // Without this the test would pass for the wrong reason: it only says
    // anything if the push was actually refused.
    let on_remote = git::run_allowing_failure(&origin, &["rev-parse", "al/job-1"])
        .await
        .unwrap();
    assert_eq!(
        on_remote.stdout.trim(),
        unrelated,
        "the push was not refused, so this test proves nothing"
    );
}

/// With no remote configured the branch simply stays local. That is a complete
/// outcome, not a degraded one, so it is still recorded as published.
#[tokio::test]
async fn publishing_without_a_remote_keeps_the_branch_local() {
    let h = Harness::new().await;

    let outcome = h.run_job("x").await;

    assert!(outcome.succeeded);
    assert!(outcome.has(
        |k| matches!(k, EventKind::JobBranchPublished { pushed_to, .. } if pushed_to.is_none())
    ));
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

    let listed = git::run_allowing_failure(
        &h.repo,
        &[
            "ls-tree",
            "--name-only",
            "-r",
            &job_branch_name(outcome.job_id),
        ],
    )
    .await
    .unwrap()
    .stdout;
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
    let body = git::run_allowing_failure(&h.repo, &["show", &format!("{branch}:rounds.txt")])
        .await
        .unwrap()
        .stdout;
    assert!(
        body.starts_with("hi\n"),
        "round 1's line is gone, so round 2 started from scratch: {body}"
    );
    assert!(
        body.contains("add error handling"),
        "round 2 never saw the feedback: {body}"
    );
}
