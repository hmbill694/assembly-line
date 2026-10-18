//! One repository, one committed `.assembly/config.toml`, one job.
//!
//! Shared by every test that needs a real git repository to run an agent
//! against. `mod support;` compiles a private copy into each test binary, so
//! items only some of them use are expected to look unused here.
#![allow(dead_code)]

pub mod daemon;

use assembly_line::claim;
use assembly_line::config::RepoConfig;
use assembly_line::event::EventKind;
use assembly_line::frame::{FrameWriter, ReadableFrames};
use assembly_line::git;
use assembly_line::job::JobId;
use assembly_line::paths::{self, JobPaths};
use assembly_line::run::{RunRefused, RunRequest, prepare_run};
use assembly_line::runner::LaunchSpec;
use assembly_line::runner::local::LocalRunner;
use assembly_line::state::JobState;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;
use tokio_util::sync::CancellationToken;

/// The job id of [`Harness::job_paths`]. A job run through
/// [`Harness::run_job`] gets whatever id it claims.
const THE_JOB: u64 = 1;

/// Path to one of the fake-agent scripts under `tests/fixtures`.
pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// `PATH` with a `gh` in front that always refuses. A passing job delivers by
/// running `gh pr create`, and a test must never reach the developer's real
/// `gh`, which is signed in and on the network.
pub fn path_where_gh_refuses() -> String {
    format!(
        "{}:{}",
        fixture("no-gh").display(),
        std::env::var("PATH").unwrap_or_default()
    )
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

/// Stage everything in `repo` and commit it. `None` means the tree was clean.
pub async fn commit_all(repo: &Path, message: &str) -> anyhow::Result<Option<String>> {
    git::commit_all(repo, message).await
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

/// A repository that has opted in, with a remote its jobs push to.
pub struct Harness {
    tmp: tempfile::TempDir,
    /// The repository a job runs against.
    pub repo: PathBuf,
    /// The bare remote the repository's `main` is published to, which every
    /// job clones from and pushes its branch back to.
    pub origin: PathBuf,
    /// A state root for tests that keep a job's state the way a host does.
    /// [`Harness::run_job`] keeps none.
    pub root: PathBuf,
}

impl Harness {
    /// A repository whose committed config runs the passing fake agent.
    pub async fn new() -> Self {
        Self::with_config(&config_running("fake-agent.sh")).await
    }

    /// A repository whose `.assembly/config.toml` is `body`, committed so it
    /// can be read from a ref — which is the only way a job ever reads it.
    ///
    /// Delivery is turned off: these jobs are about what a round does, and
    /// `tests/cli.rs` and `tests/delivery.rs` cover what happens to its branch.
    pub async fn with_config(body: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_git_repo(&repo).await;

        std::fs::create_dir_all(repo.join(".assembly")).unwrap();
        std::fs::write(
            repo.join(assembly_line::config::REPO_CONFIG_PATH),
            format!("{body}\n[delivery]\nmode = \"none\"\n"),
        )
        .unwrap();
        commit_all(&repo, "opt in to assembly-line")
            .await
            .unwrap()
            .unwrap();

        let origin = tmp.path().join("origin.git");
        add_origin(&repo, &origin).await;
        publish_main(&repo).await;

        // Tests hand this to child processes as their TMPDIR.
        std::fs::create_dir_all(tmp.path().join("scratch")).unwrap();
        Harness {
            root: tmp.path().join("root"),
            tmp,
            repo,
            origin,
        }
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

    /// A job directory outside the repository, for tests that drive a runner
    /// directly and collect what it reports.
    pub fn job_paths(&self) -> JobPaths {
        let jobs_dir = self.tmp.path().join("jobs");
        paths::open_job(&jobs_dir, THE_JOB.into())
            .or_else(|_| paths::create_job(&jobs_dir, THE_JOB.into()))
            .unwrap()
    }

    /// Run one job against this repository, with `prompt`, from `main` —
    /// in this process, as `assembly run` does.
    pub async fn run_job(&self, prompt: &str) -> Outcome {
        self.run(self.request(prompt, None, None)).await
    }

    /// Run one job cut from a named ref rather than `main`.
    pub async fn run_job_from(&self, prompt: &str, base_ref: &str) -> Outcome {
        self.run(self.request(prompt, None, Some(base_ref))).await
    }

    /// Another round on job `job_id`, continuing its branch.
    pub async fn revise_job(&self, job_id: JobId, prompt: &str) -> Outcome {
        self.run(RunRequest {
            job: Some(job_id.into()),
            ..self.request(prompt, None, None)
        })
        .await
    }

    /// Why a job with `provider` would not start: a refusal before anything
    /// was claimed, as distinct from a job that failed.
    pub async fn refusal_to_start(&self, prompt: &str, provider: Option<&str>) -> RunRefused {
        match prepare_run(
            self.request(prompt, provider, None),
            &self.scratch_root(),
            &CancellationToken::new(),
        )
        .await
        {
            Ok(_) => panic!("the job was ready to run, not refused"),
            Err(refused) => refused,
        }
    }

    /// A claimed job and the launch spec for its first round, for tests
    /// that hand a round to a runner directly.
    pub async fn launch_spec_for(&self, prompt: &str) -> LaunchSpec {
        let base = git::pinned(&self.repo, "origin", "main").await.unwrap();
        let config = RepoConfig::from_ref(&self.repo, &base.sha).await.unwrap();
        let job = claim::claim_job(&self.repo, "origin", &base.sha)
            .await
            .unwrap();
        LaunchSpec::for_round::<LocalRunner>(
            &JobPaths {
                id: job,
                dir: self.tmp.path().join("jobs").join(job.to_string()),
            },
            1,
            self.origin.to_str().unwrap(),
            &base,
            prompt,
            config.provider.as_deref().unwrap_or_default(),
            None,
        )
    }

    /// `assembly` run by hand with a launch spec's arguments, as a runner
    /// would run it, with `extra_env` on top of this process's environment.
    pub async fn run_frames(&self, prompt: &str, extra_env: &[(&str, &str)]) -> Output {
        std::process::Command::new(env!("CARGO_BIN_EXE_assembly"))
            .args(self.launch_spec_for(prompt).await.args)
            .env("TMPDIR", self.scratch_root())
            .envs(extra_env.iter().copied())
            .output()
            .unwrap()
    }

    fn request(&self, prompt: &str, provider: Option<&str>, base_ref: Option<&str>) -> RunRequest {
        RunRequest {
            repo: Some(self.repo.to_string_lossy().into_owned()),
            base_ref: base_ref.map(str::to_string),
            prompt: Some(prompt.to_string()),
            provider: provider.map(str::to_string),
            ..RunRequest::default()
        }
    }

    /// Run a job that must be ready rather than refused, its frames kept in
    /// memory.
    async fn run(&self, request: RunRequest) -> Outcome {
        let cancel = CancellationToken::new();
        let ready = match prepare_run(request, &self.scratch_root(), &cancel).await {
            Ok(ready) => ready,
            Err(refused) => panic!(
                "the job was refused: {refused} {:?}",
                refused.itemized_reasons()
            ),
        };
        let frames = FrameWriter::new(Vec::new());
        let conclusion = ready.run(&frames, cancel).await.unwrap();
        let mut readable = ReadableFrames::new(Vec::new());
        readable.write_all(&frames.copy_of_sink()).unwrap();

        Outcome {
            passed: conclusion.verdict.passed(),
            job_id: conclusion.job,
            state: conclusion.report.state,
            events: frames.events_so_far().into_iter().map(|e| e.kind).collect(),
            output: String::from_utf8(readable.into_text()).unwrap(),
        }
    }
}

/// What a finished job reported.
#[derive(Debug)]
pub struct Outcome {
    pub passed: bool,
    pub job_id: JobId,
    pub state: JobState,
    /// The events of this run alone: an earlier round's are not among them.
    pub events: Vec<EventKind>,
    /// Every output line the run printed: its commands', and its own.
    pub output: String,
}

impl Outcome {
    pub fn has(&self, predicate: impl Fn(&EventKind) -> bool) -> bool {
        self.events.iter().any(predicate)
    }
}
