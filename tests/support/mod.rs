//! One repository, one committed `.assembly/config.toml`, one job.
//!
//! Shared by every test that needs a real git repository to run an agent
//! against. `mod support;` compiles a private copy into each test binary, so
//! items only some of them use are expected to look unused here.
#![allow(dead_code)]

use assembly_line::config::RepoConfig;
use assembly_line::event::{EventKind, EventLog};
use assembly_line::frame::{FrameWriter, Routed, StreamPosition};
use assembly_line::git::{self, commit_all};
use assembly_line::job::run_round;
use assembly_line::paths::{self, JobPaths};
use assembly_line::payload::{self, JobPayload, RoundRequest};
use assembly_line::state::JobState;
use assembly_line::workspace;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

/// The job id every harness uses. One repository per test, so there is never
/// a second job to collide with.
const THE_JOB: u64 = 1;

/// Path to one of the fake-agent scripts under `tests/fixtures`.
pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Write an executable shell script called `name` into `dir` — a stand-in
/// for a CLI such as `docker`, so no test ever reaches the real one.
pub fn fake_cli(dir: &Path, name: &str, body: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    std::fs::write(
        &path,
        format!("#!/usr/bin/env bash\nset -euo pipefail\n{body}"),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A `[providers.fake]` block that runs one of the fixture scripts.
/// `extra_arg` reaches the script as `$2`, distinguishing two instances.
pub fn provider_block(script: &str, extra_arg: &str) -> String {
    format!(
        "[providers.fake]\ncmd = \"bash\"\nargs = [\"{}\", \"{{prompt}}\", \"{extra_arg}\"]\n",
        fixture(script).display()
    )
}

/// A repository config naming `fake` and pointing it at `script`.
pub fn config_running(script: &str) -> String {
    format!("provider = \"fake\"\n{}", provider_block(script, "a"))
}

/// Turn `at` into a git repository with one commit, so a job has somewhere to
/// branch from. The one definition of "a git repo with a commit in it".
pub async fn init_git_repo(at: &Path) {
    std::fs::create_dir_all(at).unwrap();
    for args in [
        vec!["init", "--initial-branch=main"],
        vec!["config", "user.email", "test@example.com"],
        vec!["config", "user.name", "Test"],
        vec!["config", "commit.gpgsign", "false"],
    ] {
        let out = git::run_allowing_failure(at, &args).await.unwrap();
        assert!(out.succeeded(), "git {args:?} failed: {}", out.stderr);
    }
    std::fs::write(at.join("README.md"), "base\n").unwrap();
    commit_all(at, "initial").await.unwrap().unwrap();
}

/// Add a bare repository at `origin` as `repo`'s `origin` remote, so a push
/// exercises the real git path with no network and no credentials.
pub async fn add_origin(repo: &Path, origin: &Path) {
    let origin_arg = origin.to_string_lossy().into_owned();

    for args in [
        vec!["init", "--bare", "--initial-branch=main", &origin_arg],
        vec!["remote", "add", "origin", &origin_arg],
    ] {
        let out = git::run_allowing_failure(repo, &args).await.unwrap();
        assert!(out.succeeded(), "git {args:?} failed: {}", out.stderr);
    }
}

/// Push `main` to `origin`, so the remote has something a job can start from.
pub async fn publish_main(repo: &Path) {
    git::push_head_as(repo, "origin", "main").await.unwrap();
}

/// A bare repository with one commit, and nothing else. The tempdir *is* the
/// repository, so `repo.path()` is the repository root.
pub async fn repo_with_initial_commit() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    init_git_repo(tmp.path()).await;
    tmp
}

/// A repository that has opted in, plus somewhere outside it for job state.
pub struct Harness {
    tmp: tempfile::TempDir,
    /// The repository a job runs against.
    pub repo: PathBuf,
    /// The bare remote the repository's `main` is published to, which every
    /// job clones from and pushes its branch back to.
    pub origin: PathBuf,
}

impl Harness {
    /// A repository whose committed config runs the passing fake agent.
    pub async fn new() -> Self {
        Self::with_config(&config_running("fake-agent.sh")).await
    }

    /// A repository whose `.assembly/config.toml` is `body`, committed so it
    /// can be read from a ref — which is the only way a job ever reads it.
    pub async fn with_config(body: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_git_repo(&repo).await;

        std::fs::create_dir_all(repo.join(".assembly")).unwrap();
        std::fs::write(repo.join(assembly_line::config::REPO_CONFIG_PATH), body).unwrap();
        commit_all(&repo, "opt in to assembly-line")
            .await
            .unwrap()
            .unwrap();

        let origin = tmp.path().join("origin.git");
        add_origin(&repo, &origin).await;
        publish_main(&repo).await;

        Harness { tmp, repo, origin }
    }

    /// Where this harness's jobs make their scratch clones.
    pub fn scratch_root(&self) -> PathBuf {
        self.tmp.path().join("scratch")
    }

    /// Whether every scratch clone a job made is gone again.
    pub fn scratch_is_empty(&self) -> bool {
        std::fs::read_dir(self.scratch_root()).map_or(true, |mut entries| entries.next().is_none())
    }

    /// What the remote's copy of `branch` carries at `path`, or `None`.
    pub async fn file_on_remote_branch(&self, branch: &str, path: &str) -> Option<String> {
        git::file_at_ref(&self.origin, branch, path).await.unwrap()
    }

    /// The files the remote's copy of `branch` carries.
    pub async fn files_on_remote_branch(&self, branch: &str) -> String {
        git::run_allowing_failure(&self.origin, &["ls-tree", "--name-only", "-r", branch])
            .await
            .unwrap()
            .stdout
    }

    /// Job state lives outside the repository, so nothing a test does dirties
    /// the working tree it is asserting about.
    pub fn job_paths(&self) -> JobPaths {
        paths::create_job(&paths::jobs_root(self.tmp.path()), THE_JOB).unwrap()
    }

    /// Run one job against this repository, with `prompt`, from `main`.
    pub async fn run_job(&self, prompt: &str) -> Outcome {
        self.attempt_round(prompt, None, None, 1).await.unwrap()
    }

    /// Run one job cut from a named ref rather than `main`.
    pub async fn run_job_from(&self, prompt: &str, base_ref: &str) -> Outcome {
        self.attempt_round(prompt, None, Some(base_ref), 1)
            .await
            .unwrap()
    }

    /// Another round on the same job, continuing its branch.
    pub async fn revise_job(&self, prompt: &str, round: u32) -> Outcome {
        self.attempt_round(prompt, None, None, round).await.unwrap()
    }

    /// Run a job that may not be administrable at all — an undeclared
    /// provider, or an unparseable `max_duration`. Those are errors rather
    /// than failed jobs, and this is how a test sees the difference.
    ///
    /// `provider` and `base_ref` default to what the repository declares and
    /// to `main`; the wrappers above cover the ordinary cases. The start is
    /// pinned the way `main.rs` pins it: round 1 from the remote's copy of
    /// the base, a revise round from the remote's copy of the job's branch.
    pub async fn attempt_round(
        &self,
        prompt: &str,
        provider: Option<&str>,
        base_ref: Option<&str>,
        round: u32,
    ) -> anyhow::Result<Outcome> {
        let start = match round {
            1 => git::pinned(&self.repo, "origin", base_ref.unwrap_or("main")).await?,
            _ => git::pinned(&self.repo, "origin", &workspace::job_branch_name(THE_JOB)).await?,
        };
        self.round_from(&start, prompt, provider, round).await
    }

    /// The payload the host would build for round 1 of this job.
    pub async fn payload_for(&self, prompt: &str) -> JobPayload {
        let start = git::pinned(&self.repo, "origin", "main").await.unwrap();
        self.payload_from(&start, prompt, None, 1).await.unwrap()
    }

    /// Resolve a round's payload the way `main.rs` does.
    ///
    /// An undeclared provider or an unparseable `max_duration` is refused by
    /// [`JobPayload::for_round`], and propagates as the error.
    async fn payload_from(
        &self,
        start: &git::PinnedRef,
        prompt: &str,
        provider: Option<&str>,
        round: u32,
    ) -> anyhow::Result<JobPayload> {
        let config = RepoConfig::from_ref(&self.repo, &start.sha).await?;
        let provider = provider
            .map(str::to_string)
            .or_else(|| config.provider.clone())
            .unwrap_or_default();

        JobPayload::for_round(
            &config,
            RoundRequest {
                job_id: THE_JOB,
                round,
                prompt,
                provider: &provider,
                start: start.clone(),
                remote_name: workspace::DEFAULT_REMOTE,
                remote_url: payload::remote_to_clone(&self.repo, workspace::DEFAULT_REMOTE).await?,
                seed_from: &self.repo,
            },
        )
    }

    /// Everything a round does once its start is pinned: resolve a payload,
    /// run it in-process against an in-memory frame stream, and route the
    /// frames into the job's event log. A lighter collector than the host's
    /// `collect`: an in-memory stream never replays, so there is no position
    /// to carry, and a round that ends without a verdict is left without one.
    async fn round_from(
        &self,
        start: &git::PinnedRef,
        prompt: &str,
        provider: Option<&str>,
        round: u32,
    ) -> anyhow::Result<Outcome> {
        let payload = self.payload_from(start, prompt, provider, round).await?;
        let paths = self.job_paths();
        let mut log = EventLog::open_append(paths.events()).unwrap();

        let frames = FrameWriter::new(Vec::new());
        let outcome = run_round(
            &payload,
            &frames,
            &self.scratch_root(),
            CancellationToken::new(),
        )
        .await?;
        let routed: Vec<Routed> = String::from_utf8(frames.copy_of_sink())
            .unwrap()
            .lines()
            .map(|line| StreamPosition::default().route(line).1)
            .collect();
        let output: String = routed
            .iter()
            .filter_map(|r| match r {
                Routed::Output(text) => Some(format!("{text}\n")),
                _ => None,
            })
            .collect();
        routed
            .into_iter()
            .filter_map(|r| match r {
                Routed::Event { event, .. } => Some(event),
                _ => None,
            })
            .try_for_each(|event| log.append_collected(&event))
            .unwrap();
        let events = EventLog::read(paths.events()).unwrap();

        Ok(Outcome {
            succeeded: outcome.passed(),
            job_id: paths.id,
            state: JobState::replay(&events),
            events: events.into_iter().map(|e| e.kind).collect(),
            output,
        })
    }
}

/// What a finished job left in its event log.
#[derive(Debug)]
pub struct Outcome {
    pub succeeded: bool,
    pub job_id: u64,
    pub state: JobState,
    pub events: Vec<EventKind>,
    /// What the round's commands printed, one line per output frame.
    pub output: String,
}

impl Outcome {
    pub fn has(&self, predicate: impl Fn(&EventKind) -> bool) -> bool {
        self.events.iter().any(predicate)
    }
}
