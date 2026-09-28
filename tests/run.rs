//! `assembly run`: one whole job in this process, as a person types it and
//! as a runner launches it.

use assembly_line::config::REPO_CONFIG_PATH;
use assembly_line::frame::Frame;
use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use std::path::{Path, PathBuf};

mod support;

/// An opted-in repository published to a bare origin, a root nothing should
/// write to, and a scratch directory for the clone.
struct Fixture {
    tmp: tempfile::TempDir,
    repo: PathBuf,
    origin: PathBuf,
}

impl Fixture {
    async fn running(script: &str) -> Self {
        Self::with_config(&format!(
            "verify = \"true\"\n{}",
            support::config_running(script)
        ))
        .await
    }

    async fn with_config(config: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        support::init_git_repo(&repo).await;
        std::fs::create_dir_all(repo.join(".assembly")).unwrap();
        std::fs::write(
            repo.join(REPO_CONFIG_PATH),
            format!("{config}\n[delivery]\nmode = \"none\"\n"),
        )
        .unwrap();
        support::commit_all(&repo, "opt in").await.unwrap().unwrap();
        let origin = tmp.path().join("origin.git");
        support::add_origin(&repo, &origin).await;
        support::publish_main(&repo).await;
        Fixture { tmp, repo, origin }
    }

    fn root(&self) -> PathBuf {
        self.tmp.path().join("root")
    }

    /// `assembly` in the repository, with a `gh` that refuses first on
    /// `PATH`, its root and scratch inside this fixture.
    fn assembly(&self) -> Command {
        let mut cmd = Command::cargo_bin("assembly").unwrap();
        cmd.current_dir(&self.repo)
            .env("PATH", support::path_where_gh_refuses())
            .env("ASSEMBLY_ROOT", self.root())
            .env("TMPDIR", self.tmp.path().join("scratch"));
        cmd
    }

    fn on_origin(&self, args: &[&str]) -> String {
        git_in(&self.origin, args)
    }
}

fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[tokio::test]
async fn run_does_a_whole_job_and_keeps_no_state() {
    let fx = Fixture::running("fake-agent.sh").await;

    fx.assembly()
        .args(["run", "--prompt", "do the thing"])
        .assert()
        .success()
        .stdout(contains("job 1: passed").and(contains("branch: al/job-1")));

    assert!(
        fx.on_origin(&["show", "al/job-1:agent-output.txt"])
            .contains("do the thing")
    );
    assert!(!fx.root().exists(), "run kept state under the root");
    assert_eq!(git_in(&fx.repo, &["status", "--porcelain"]), "");
}

/// Given as `--prompt=value`, a prompt that looks like a flag, or spans
/// lines with quotes in them, reaches the agent byte for byte.
#[tokio::test]
async fn a_dash_prompt_given_after_an_equals_sign_reaches_the_agent_intact() {
    let fx = Fixture::running("fake-agent.sh").await;
    let prompt = "--help me \"please\"\nand 'then' -x";

    fx.assembly()
        .arg("run")
        .arg(format!("--prompt={prompt}"))
        .assert()
        .success();

    assert_eq!(fx.on_origin(&["show", "al/job-1:agent-output.txt"]), prompt);
}

/// jj leaves git's HEAD detached in a colocated repository, and a named
/// `--ref` does not need a checked-out branch.
#[tokio::test]
async fn a_detached_head_needs_a_ref_and_is_fine_with_one() {
    let fx = Fixture::running("fake-agent.sh").await;
    git_in(&fx.repo, &["checkout", "--quiet", "--detach"]);

    fx.assembly()
        .args(["run", "--prompt", "x"])
        .assert()
        .code(2)
        .stderr(contains("HEAD is detached"));

    fx.assembly()
        .args(["run", "--ref", "main", "--prompt", "x"])
        .assert()
        .success();
}

#[tokio::test]
async fn run_takes_a_remote_url_and_then_needs_a_ref() {
    let fx = Fixture::running("fake-agent.sh").await;
    let url = fx.origin.to_str().unwrap();

    fx.assembly()
        .args(["run", "--repo", url, "--prompt", "x"])
        .assert()
        .code(2)
        .stderr(contains("--ref"));

    fx.assembly()
        .args(["run", "--repo", url, "--ref", "main", "--prompt", "x"])
        .assert()
        .success();
    assert_eq!(
        fx.on_origin(&["rev-parse", "--verify", "-q", "al/job-1"])
            .len(),
        40
    );
}

#[tokio::test]
async fn with_frames_stdout_is_frames_and_nothing_else() {
    let fx = Fixture::running("fake-agent.sh").await;

    let out = fx
        .assembly()
        .args(["run", "--prompt", "x", "--frames"])
        .output()
        .unwrap();

    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    let frames: Vec<Frame> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{line}: {e}")))
        .collect();
    assert!(stdout.contains("\"t\":\"round_passed\""), "{stdout}");
    assert!(
        !stdout.contains("round_started"),
        "run numbered its own round: {stdout}"
    );
    assert_eq!(frames.first().map(|f| f.seq), Some(1));
}

/// A revise's agent gets only what to change: its earlier work is the
/// branch, and what earlier rounds were asked is in the branch's history.
#[tokio::test]
async fn run_on_a_job_continues_its_branch_with_only_the_new_prompt() {
    let fx = Fixture::running("revising-agent.sh").await;
    fx.assembly()
        .args(["run", "--prompt", "hi"])
        .assert()
        .success();

    fx.assembly()
        .args(["run", "--job", "1", "--prompt", "add error handling"])
        .assert()
        .success()
        .stdout(contains("job 1: passed"));

    assert_eq!(fx.on_origin(&["rev-list", "--count", "al/job-1"]), "4");
    let rounds = fx.on_origin(&["show", "al/job-1:rounds.txt"]);
    assert_eq!(rounds, "hi\nadd error handling", "{rounds}");
}

#[tokio::test]
async fn run_on_a_job_with_no_branch_is_refused() {
    let fx = Fixture::running("fake-agent.sh").await;

    fx.assembly()
        .args(["run", "--job", "9", "--prompt", "x"])
        .assert()
        .code(2)
        .stderr(contains("job 9 has no branch"));
}

/// Refused before the claim: a broken config costs no id and leaves no
/// branch on the remote.
#[tokio::test]
async fn a_config_that_cannot_run_is_refused_before_anything_is_claimed() {
    let fx = Fixture::with_config("provider = \"ghost\"\nmax_duration = \"soon\"").await;

    fx.assembly()
        .args(["run", "--prompt", "x"])
        .assert()
        .code(2)
        .stderr(contains("'ghost'").and(contains("max_duration 'soon'")));

    assert_eq!(fx.on_origin(&["branch", "--list", "al/job-*"]), "");
}

#[tokio::test]
async fn a_pinned_base_is_used_even_after_the_ref_moved() {
    let fx = Fixture::running("fake-agent.sh").await;
    let pinned = git_in(&fx.repo, &["rev-parse", "HEAD"]);
    std::fs::write(fx.repo.join("later.txt"), "later\n").unwrap();
    support::commit_all(&fx.repo, "later")
        .await
        .unwrap()
        .unwrap();
    support::publish_main(&fx.repo).await;

    fx.assembly()
        .args(["run", "--ref", &format!("main@{pinned}"), "--prompt", "x"])
        .assert()
        .success();

    assert_eq!(fx.on_origin(&["rev-parse", "al/job-1^"]), pinned);
}

#[tokio::test]
async fn the_agent_never_sees_the_forge_token() {
    let fx = Fixture::running("forge-token-reporting-agent.sh").await;

    fx.assembly()
        .env("GH_TOKEN", "s3cret")
        .args(["run", "--prompt", "x"])
        .assert()
        .success();

    assert_eq!(
        fx.on_origin(&["show", "al/job-1:forge-token.txt"]),
        "absent"
    );
}
