use assembly_line::config::{ConfigError, REPO_CONFIG_PATH};
use assembly_line::event::EventKind;
use assembly_line::git::{self, head_sha};
use assembly_line::lifecycle::Refusal;
use assembly_line::payload;
use assembly_line::state::JobState;
use assembly_line::workspace::job_branch_name;
use support::{Harness, commit_all, config_running, provider_block};

mod support;

/// The contract the whole milestone rests on: the checkout is scratch, the
/// branch is the durable artifact.
#[tokio::test]
async fn a_job_leaves_one_branch_carrying_its_work() {
    let h = Harness::new().await;

    let outcome = h.run_job("write a file").await;

    assert!(outcome.passed);
    assert_eq!(outcome.state, JobState::Passed);
    assert!(
        outcome.has(|e| matches!(e, EventKind::RoundCommitted { .. })),
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
        outcome.passed,
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

    assert!(outcome.passed);
    assert_eq!(head_sha(&h.repo).await.unwrap(), base);
    assert_eq!(
        git::current_branch(&h.repo).await.unwrap().as_deref(),
        Some("main")
    );
    assert!(!h.repo.join("agent-output.txt").exists());
    let status = git::run_allowing_failure(&h.repo, &["status", "--porcelain"])
        .await
        .unwrap()
        .stdout;
    assert!(status.is_empty(), "{status}");

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

    assert!(!outcome.passed);
    assert_eq!(outcome.state, JobState::Failed);
    assert!(
        outcome
            .has(|k| matches!(k, EventKind::RoundFailed { reason } if reason.contains("exit 3")))
    );
    assert!(outcome.has(|k| matches!(k, EventKind::RoundCommitted { .. })));

    let branch = job_branch_name(outcome.job_id);
    assert!(
        outcome
            .has(|k| matches!(k, EventKind::BranchPushed { branch: b, pushed_to } if *b == branch && pushed_to == "origin"))
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

/// The checkout is the agent's to change, permissions included. One it has
/// made hard to delete still goes, and the work it holds is still recorded.
#[tokio::test]
async fn a_checkout_the_agent_write_protected_is_still_discarded_and_its_work_kept() {
    let h = Harness::with_config(&config_running("locking-agent.sh")).await;

    let outcome = h.run_job("x").await;

    assert!(outcome.passed, "{:?}", outcome.state);
    assert!(outcome.has(|k| matches!(k, EventKind::RoundCommitted { .. })));
    assert!(
        h.files_on_remote_branch(&job_branch_name(outcome.job_id))
            .await
            .contains("locked/work.txt")
    );
    assert!(
        h.scratch_is_empty(),
        "the write-protected checkout survived"
    );
}

/// The branch is pushed before `verify` runs, so a `verify` that cannot even
/// start must not take the round's record of that branch down with it.
#[tokio::test]
async fn a_verify_that_cannot_start_still_records_the_pushed_branch() {
    let h = Harness::new().await;
    // No `sh` on PATH, so `verify` cannot be spawned; the agent runs under
    // an absolute `/bin/bash` and git is found through its own link.
    let only_git = h.scratch_root().with_file_name("only-git");
    std::fs::create_dir_all(&only_git).unwrap();
    let git_binary = std::env::var("PATH")
        .unwrap()
        .split(':')
        .map(|dir| std::path::Path::new(dir).join("git"))
        .find(|candidate| candidate.is_file())
        .unwrap();
    std::os::unix::fs::symlink(git_binary, only_git.join("git")).unwrap();
    let base = h.payload_for("x").await;
    let command = assembly_line::provider::CommandSpec {
        program: "/bin/bash".into(),
        ..base.command.clone()
    };
    let payload = payload::RoundPayload {
        command,
        verify: Some("true".into()),
        ..base
    };

    let output = std::process::Command::new(assert_cmd::cargo::cargo_bin("assembly"))
        .arg("job-exec")
        .env(
            payload::PAYLOAD_VAR,
            serde_json::to_string(&payload).unwrap(),
        )
        .env("PATH", &only_git)
        .env("TMPDIR", h.scratch_root())
        .output()
        .unwrap();

    let frames = String::from_utf8_lossy(&output.stdout);
    assert!(
        frames.contains("\"t\":\"branch_pushed\""),
        "the pushed branch went unrecorded: {frames}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(frames.contains("verify could not run"), "{frames}");
}

/// The agent leads a session of its own, so a terminal's hangup never
/// reaches it: `job-exec` has to cancel the round itself, or the agent runs
/// on with nothing left to stop it.
#[tokio::test]
async fn a_hangup_cancels_the_round_rather_than_orphaning_the_agent() {
    use std::io::BufRead;

    let h = Harness::with_config(&config_running("sleeping-agent.sh")).await;
    let payload = h.payload_for("x").await;

    let mut job_exec = std::process::Command::new(assert_cmd::cargo::cargo_bin("assembly"))
        .arg("job-exec")
        .env(
            payload::PAYLOAD_VAR,
            serde_json::to_string(&payload).unwrap(),
        )
        .env("TMPDIR", h.scratch_root())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut frames = std::io::BufReader::new(job_exec.stdout.take().unwrap()).lines();
    let before_hangup: Vec<String> = frames
        .by_ref()
        .map(Result::unwrap)
        .take_while(|line| !line.contains("sleeping-agent:"))
        .collect();
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(job_exec.id()).unwrap()),
        nix::sys::signal::Signal::SIGHUP,
    )
    .unwrap();
    let after_hangup: String = frames.map(Result::unwrap).collect();
    job_exec.wait().unwrap();

    assert!(
        after_hangup.contains("\"reason\":\"cancelled\""),
        "the round never reported itself: {before_hangup:?} {after_hangup}"
    );
    assert!(h.scratch_is_empty(), "the scratch clone was left behind");
}

/// A job whose branch cannot leave the scratch clone has lost its work, and
/// must say so rather than record a branch that exists nowhere.
#[tokio::test]
async fn a_refused_push_fails_the_job_and_says_the_work_is_lost() {
    let h = Harness::new().await;
    let prepared = h.prepare_job("write a file").await;

    // Someone else publishes an unrelated `al/job-1` after this job looked at
    // the remote's branches but before it pushes, so its push is a
    // non-fast-forward and git refuses it.
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

    let outcome = h.run(prepared).await;

    assert!(!outcome.passed);
    assert!(
        !outcome.has(|k| matches!(k, EventKind::BranchPushed { .. })),
        "a branch that never left the clone was recorded as published: {:?}",
        outcome.events
    );
    assert!(outcome.has(
        |k| matches!(k, EventKind::RoundFailed { reason } if reason.contains("work is lost"))
    ));
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
async fn an_agent_that_changes_nothing_passes_without_committing() {
    let h = Harness::with_config(&config_running("noop-agent.sh")).await;

    let outcome = h.run_job("x").await;

    assert!(outcome.passed);
    assert_eq!(outcome.state, JobState::Passed);
    assert!(!outcome.has(|k| matches!(k, EventKind::RoundCommitted { .. })));
}

/// A clean tree is not proof of an idle agent: one that commits its own work
/// leaves nothing to stage, and the scratch clone holding those commits is
/// about to be deleted.
#[tokio::test]
async fn an_agent_that_commits_its_own_work_has_it_published() {
    let h = Harness::with_config(&config_running("committing-agent.sh")).await;

    let outcome = h.run_job("self-committed").await;

    assert!(outcome.passed);
    assert!(outcome.has(|k| matches!(k, EventKind::RoundCommitted { .. })));
    assert_eq!(
        h.file_on_remote_branch(&job_branch_name(outcome.job_id), "agent-output.txt")
            .await
            .as_deref(),
        Some("self-committed\n")
    );
}

/// What reaches the remote is the commit the round inspected — the one
/// checked out — not whatever the job's local branch was left pointing at.
#[tokio::test]
async fn an_agent_that_switches_branches_publishes_what_it_left_checked_out() {
    let h = Harness::with_config(&format!(
        "provider = \"fake\"\ncopy = [\".env\"]\n{}",
        provider_block("branch-switching-agent.sh", "a")
    ))
    .await;
    std::fs::write(h.repo.join(".env"), "API_KEY=hunter2\n").unwrap();

    let outcome = h.run_job("switched").await;
    assert!(outcome.passed);

    let branch = job_branch_name(outcome.job_id);
    assert_eq!(
        h.file_on_remote_branch(&branch, "agent-output.txt")
            .await
            .as_deref(),
        Some("switched\n")
    );
    let history =
        git::run_allowing_failure(&h.origin, &["log", "--format=", "--name-only", &branch])
            .await
            .unwrap()
            .stdout;
    assert!(
        !history.contains(".env"),
        "the seeded secret reached the remote: {history}"
    );
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
    assert!(outcome.passed);

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

    assert!(!outcome.passed);
    assert!(
        outcome.has(
            |k| matches!(k, EventKind::RoundFailed { reason } if reason.contains("definitely-not-real-xyz"))
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

    let refusal = h.refusal_to_start("x", Some("ghost")).await;

    assert!(
        matches!(
            &refusal,
            Refusal::ConfigNotRunnable(errors)
                if errors == &[ConfigError::UnknownProvider("ghost".into())]
        ),
        "{refusal:?}"
    );
    assert!(!h.repo.join(".assembly/jobs").exists());
}

#[tokio::test]
async fn an_unparseable_max_duration_stops_the_job_before_it_starts() {
    let h = Harness::with_config(&format!(
        "provider = \"fake\"\nmax_duration = \"soon\"\n{}",
        provider_block("fake-agent.sh", "a")
    ))
    .await;

    let refusal = h.refusal_to_start("x", None).await;

    assert!(
        matches!(
            &refusal,
            Refusal::ConfigNotRunnable(errors)
                if errors == &[ConfigError::UnparseableMaxDuration("soon".into())]
        ),
        "{refusal:?}"
    );
}

/// The heart of the stateless design: a revise round is a new job that sees
/// its prior work because that work *is* the branch it starts from. Nothing
/// was kept on disk between the rounds.
#[tokio::test]
async fn a_revise_round_continues_the_branch_instead_of_starting_over() {
    let h = Harness::with_config(&config_running("revising-agent.sh")).await;

    let first = h.run_job("hi").await;
    assert!(first.passed);

    let second = h.revise_job(first.job_id, "add error handling").await;
    assert!(second.passed);

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

/// `git` leads a session of its own, so no terminal signal reaches a clone
/// in progress: only the round's cancel can stop it. SIGTERM is what the
/// local runner sends `job-exec` on Ctrl-C.
#[tokio::test]
async fn cancelling_a_round_stops_a_clone_in_progress() {
    let h = Harness::new().await;
    let fakes = h.scratch_root().with_file_name("fakes");
    support::fake_cli(&fakes, "git-remote-hang", "sleep 60\n");
    let payload = payload::RoundPayload {
        remote_url: "hang::nowhere".into(),
        ..h.payload_for("x").await
    };

    let started = std::time::Instant::now();
    let job_exec = std::process::Command::new(assert_cmd::cargo::cargo_bin("assembly"))
        .arg("job-exec")
        .env(
            payload::PAYLOAD_VAR,
            serde_json::to_string(&payload).unwrap(),
        )
        .env(
            "PATH",
            format!("{}:{}", fakes.display(), std::env::var("PATH").unwrap()),
        )
        .env("TMPDIR", h.scratch_root())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_secs(1));
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(job_exec.id()).unwrap()),
        nix::sys::signal::Signal::SIGTERM,
    )
    .unwrap();
    let output = job_exec.wait_with_output().unwrap();

    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "the clone ran on past its cancel: {:?}",
        started.elapsed()
    );
    let frames = String::from_utf8_lossy(&output.stdout);
    assert!(frames.contains("\"reason\":\"cancelled\""), "{frames}");
}
