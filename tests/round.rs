use assembly_line::config::{ConfigError, REPO_CONFIG_PATH};
use assembly_line::event::EventKind;
use assembly_line::git::{self, head_sha};
use assembly_line::job::JobId;
use assembly_line::payload;
use assembly_line::run::RunRefused;
use assembly_line::runner::LaunchSpec;
use assembly_line::runner::local::LocalRunner;
use assembly_line::state::JobState;
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
        !h.files_on_remote_branch(&outcome.job_id.branch_name())
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

    let branch = outcome.job_id.branch_name();
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
        .files_on_remote_branch(&outcome.job_id.branch_name())
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
        .files_on_remote_branch(&outcome.job_id.branch_name())
        .await;
    assert!(listed.contains("agent-output.txt"), "{listed}");
}

#[tokio::test]
async fn the_prompt_reaches_the_agent_intact() {
    let h = Harness::new().await;

    let prompt = "quotes \" and $HOME and ; semicolons";
    let outcome = h.run_job(prompt).await;

    let content = h
        .file_on_remote_branch(&outcome.job_id.branch_name(), "agent-output.txt")
        .await
        .expect("the agent's output reached the branch");
    // Equality, not `contains`: the quote is a hazard too, and a `contains`
    // pair would pass with it stripped.
    assert_eq!(content.trim_end_matches('\n'), prompt);
}

#[tokio::test]
async fn the_branch_remembers_what_its_round_was_asked() {
    let h = Harness::new().await;
    let prompt = "write a file\n\nin the root, please";

    let outcome = h.run_job(prompt).await;
    assert!(outcome.passed);

    let body = git::run_allowing_failure(
        &h.origin,
        &["log", "-1", "--format=%B", &outcome.job_id.branch_name()],
    )
    .await
    .unwrap()
    .stdout;
    assert!(body.contains("in the root, please"), "{body}");
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

    let branch = outcome.job_id.branch_name();
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
        h.files_on_remote_branch(&outcome.job_id.branch_name())
            .await
            .contains("locked/work.txt")
    );
    assert!(
        h.scratch_is_empty(),
        "the write-protected checkout survived"
    );
}

/// Review focus 1, end to end: a prompt shaped like a flag, with quotes and
/// a newline in it, reaches the agent exactly.
#[tokio::test]
async fn a_prompt_that_looks_like_a_flag_reaches_the_agent_intact() {
    let h = Harness::new().await;
    let prompt = "--frames --job=9\nsay \"hi\" and 'bye'";

    let outcome = h.run_job(prompt).await;

    assert!(outcome.passed, "{:?}", outcome.events);
    assert_eq!(
        h.file_on_remote_branch(&outcome.job_id.branch_name(), "agent-output.txt")
            .await
            .as_deref(),
        Some(format!("{prompt}\n").as_str())
    );
}

/// A remote that is itself a checkout, with a remote of its own, is where
/// the job goes — not that checkout's remote.
#[tokio::test]
async fn a_remote_that_is_a_checkout_is_used_rather_than_its_own_remote() {
    let h = Harness::new().await;
    let checkout_remote = h.scratch_root().with_file_name("checkout-remote");
    let cloned = git::run_allowing_failure(
        &h.repo,
        &[
            "clone",
            "--quiet",
            h.origin.to_str().unwrap(),
            checkout_remote.to_str().unwrap(),
        ],
    )
    .await
    .unwrap();
    assert!(cloned.succeeded(), "{}", cloned.stderr);
    let repointed = git::run_allowing_failure(
        &h.repo,
        &[
            "remote",
            "set-url",
            "origin",
            checkout_remote.to_str().unwrap(),
        ],
    )
    .await
    .unwrap();
    assert!(repointed.succeeded(), "{}", repointed.stderr);

    let outcome = h.run_job("write a file").await;

    assert!(outcome.passed, "{:?} {}", outcome.events, outcome.output);
    let branch = outcome.job_id.branch_name();
    assert!(
        git::file_at_ref(&checkout_remote, &branch, "agent-output.txt")
            .await
            .unwrap()
            .is_some(),
        "the job's work is not on the remote it was given"
    );
    assert_eq!(
        h.file_on_remote_branch(&branch, "agent-output.txt").await,
        None,
        "the job went to its remote's remote"
    );
}

/// The branch is pushed before `verify` runs, so a `verify` that cannot even
/// start must not take the round's record of that branch down with it.
#[tokio::test]
async fn a_verify_that_cannot_start_still_records_the_pushed_branch() {
    // The agent runs under an absolute `/bin/bash`, so it needs no PATH.
    let h = Harness::with_config(&format!(
        "provider = \"fake\"\nverify = \"true\"\n\
         [providers.fake]\ncmd = \"/bin/bash\"\nargs = [\"{}\", \"{{prompt}}\", \"a\"]\n",
        support::fixture("fake-agent.sh").display()
    ))
    .await;
    // No `sh` on PATH, so `verify` cannot be spawned; git is found through
    // its own link.
    let only_git = h.scratch_root().with_file_name("only-git");
    std::fs::create_dir_all(&only_git).unwrap();
    let git_binary = std::env::var("PATH")
        .unwrap()
        .split(':')
        .map(|dir| std::path::Path::new(dir).join("git"))
        .find(|candidate| candidate.is_file())
        .unwrap();
    std::os::unix::fs::symlink(git_binary, only_git.join("git")).unwrap();

    let output = h
        .run_frames("x", &[("PATH", only_git.to_str().unwrap())])
        .await;

    let frames = String::from_utf8_lossy(&output.stdout);
    assert!(
        frames.contains("\"t\":\"branch_pushed\""),
        "the pushed branch went unrecorded: {frames}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(frames.contains("verify could not run"), "{frames}");
}

/// The agent leads a session of its own, so a terminal's hangup never
/// reaches it: `run` has to cancel the round itself, or the agent runs on
/// with nothing left to stop it.
#[tokio::test]
async fn a_hangup_cancels_the_round_rather_than_orphaning_the_agent() {
    use std::io::BufRead;

    let h = Harness::with_config(&config_running("sleeping-agent.sh")).await;

    let mut run = std::process::Command::new(assert_cmd::cargo::cargo_bin("assembly"))
        .args(h.launch_spec_for("x").await.args)
        .env("TMPDIR", h.scratch_root())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut frames = std::io::BufReader::new(run.stdout.take().unwrap()).lines();
    let before_hangup: Vec<String> = frames
        .by_ref()
        .map(Result::unwrap)
        .take_while(|line| !line.contains("sleeping-agent:"))
        .collect();
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(run.id()).unwrap()),
        nix::sys::signal::Signal::SIGHUP,
    )
    .unwrap();
    let after_hangup: String = frames.map(Result::unwrap).collect();
    run.wait().unwrap();

    assert!(
        after_hangup.contains("\"reason\":\"cancelled\""),
        "the round never reported itself: {before_hangup:?} {after_hangup}"
    );
    assert!(h.scratch_is_empty(), "the scratch clone was left behind");
}

/// A job whose branch cannot leave the scratch clone has lost its work, and
/// must say so rather than record a branch that exists nowhere.
#[tokio::test]
async fn a_refused_push_fails_the_round_and_says_the_work_is_lost() {
    let h = Harness::new().await;
    // The remote lets a branch be created — the job's claim — and refuses
    // every update after that, so the round's push is the one refused.
    support::fake_cli(
        &h.origin.join("hooks"),
        "pre-receive",
        "while read -r old _ _; do\n  \
           [[ \"$old\" =~ ^0+$ ]] || { echo 'only new branches here' >&2; exit 1; }\n\
         done\n",
    );
    let base = head_sha(&h.repo).await.unwrap();

    let outcome = h.run_job("write a file").await;

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
        base,
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
        h.file_on_remote_branch(&outcome.job_id.branch_name(), "agent-output.txt")
            .await
            .as_deref(),
        Some("self-committed\n")
    );
}

/// What reaches the remote is the commit the round inspected — the one
/// checked out — not whatever the job's local branch was left pointing at.
#[tokio::test]
async fn an_agent_that_switches_branches_publishes_what_it_left_checked_out() {
    let h = Harness::with_config(&config_running("branch-switching-agent.sh")).await;

    let outcome = h.run_job("switched").await;
    assert!(outcome.passed);

    assert_eq!(
        h.file_on_remote_branch(&outcome.job_id.branch_name(), "agent-output.txt")
            .await
            .as_deref(),
        Some("switched\n")
    );
}

#[tokio::test]
async fn a_missing_provider_binary_fails_the_round_with_a_useful_message() {
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
            RunRefused::ConfigNotRunnable(errors)
                if errors == &[ConfigError::UnknownProvider("ghost".into())]
        ),
        "{refusal:?}"
    );
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
            RunRefused::ConfigNotRunnable(errors)
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

    let branch = second.job_id.branch_name();
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
/// local runner sends `run` on Ctrl-C.
#[tokio::test]
async fn cancelling_a_round_stops_a_clone_in_progress() {
    let h = Harness::new().await;
    let fakes = h.scratch_root().with_file_name("fakes");
    let cloning = fakes.join("cloning");
    support::fake_cli(
        &fakes,
        "git-remote-hang",
        &format!("touch {}\nsleep 60\n", cloning.display()),
    );
    let spec = LaunchSpec::for_round::<LocalRunner>(
        JobId::from(1),
        1,
        "hang::nowhere",
        &git::pinned(&h.repo, "origin", "main").await.unwrap(),
        "x",
        "fake",
        None,
    );

    let mut run = std::process::Command::new(assert_cmd::cargo::cargo_bin("assembly"))
        .args(&spec.args)
        .env(
            "PATH",
            format!("{}:{}", fakes.display(), std::env::var("PATH").unwrap()),
        )
        .env("TMPDIR", h.scratch_root())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let spawned = std::time::Instant::now();
    while !cloning.exists() {
        match (
            run.try_wait().unwrap(),
            spawned.elapsed() > std::time::Duration::from_secs(15),
        ) {
            (Some(_), _) => panic!(
                "`run` ended before it began cloning: {}",
                String::from_utf8_lossy(&run.wait_with_output().unwrap().stderr)
            ),
            (None, true) => {
                let _ = run.kill();
                panic!("the clone never reached the remote helper");
            }
            (None, false) => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    }

    let cancelled = std::time::Instant::now();
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(run.id()).unwrap()),
        nix::sys::signal::Signal::SIGTERM,
    )
    .unwrap();
    let output = run.wait_with_output().unwrap();

    assert!(
        cancelled.elapsed() < std::time::Duration::from_secs(20),
        "the clone ran on past its cancel: {:?}",
        cancelled.elapsed()
    );
    // The clone is `run`'s preparation, so a cancel there is a refusal,
    // reported on stderr, rather than a round.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error: cancelled"), "{stderr}");
}
