//! One repository, one committed `.assembly/config.toml`, one job.
//!
//! Shared by every test that needs a real git repository to run an agent
//! against. `mod support;` compiles a private copy into each test binary, so
//! items only some of them use are expected to look unused here.
#![allow(dead_code)]

use assembly_line::config::RepoConfig;
use assembly_line::event::{EventKind, EventLog};
use assembly_line::git::{self, commit_all};
use assembly_line::paths::{self, JobPaths};
use assembly_line::scheduler::{JobSpec, RunOpts, run_job};
use assembly_line::state::JobState;
use assembly_line::workspace;
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
}

/// Worktrees live under `$HOME`. A job discards its own, but a job that dies
/// mid-round can still orphan one, and tests must not leave that behind.
impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(root) = paths::repo_worktrees_root(&self.repo) {
            let _ = std::fs::remove_dir_all(root);
        }
    }
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

        Harness { tmp, repo }
    }

    pub async fn with_origin(&self) -> PathBuf {
        let origin = self.tmp.path().join("origin.git");
        add_origin(&self.repo, &origin).await;
        origin
    }

    /// The repository's configuration as the job will read it.
    pub async fn repo_config(&self) -> RepoConfig {
        RepoConfig::from_ref(&self.repo, "HEAD").await.unwrap()
    }

    /// Job state lives outside the repository, so nothing a test does dirties
    /// the working tree it is asserting about.
    pub fn job_paths(&self) -> JobPaths {
        paths::create_job(&paths::jobs_root(self.tmp.path()), THE_JOB).unwrap()
    }

    /// Where the job's scratch checkout lives, which must never survive it.
    pub fn worktree_root(&self) -> PathBuf {
        paths::worktree_root(&self.repo, THE_JOB).expect("HOME is set in the test environment")
    }

    /// Run one job against this repository, with `prompt`, from `HEAD`.
    pub async fn run_job(&self, prompt: &str) -> Outcome {
        self.attempt_round(prompt, None, None, 1).await.unwrap()
    }

    /// Run one job cut from a named ref rather than `HEAD`.
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
    /// provider, an unparseable `max_duration`, a repository with no commits.
    /// Those are errors rather than failed jobs, and this is how a test sees
    /// the difference.
    ///
    /// `provider` and `base_ref` default to what the repository declares and
    /// to `HEAD`; the wrappers above cover the ordinary cases.
    pub async fn attempt_round(
        &self,
        prompt: &str,
        provider: Option<&str>,
        base_ref: Option<&str>,
        round: u32,
    ) -> anyhow::Result<Outcome> {
        let config = self.repo_config().await;
        let provider = provider
            .map(str::to_string)
            .or_else(|| config.provider.clone())
            .unwrap_or_default();
        let paths = self.job_paths();
        let mut log = EventLog::open_append(paths.events()).unwrap();

        let opts = RunOpts {
            cancel: CancellationToken::new(),
            repo: self.repo.clone(),
            seed_from: self.repo.clone(),
            remote: workspace::DEFAULT_REMOTE.to_string(),
        };
        let spec = JobSpec {
            prompt,
            provider: &provider,
            base_ref: base_ref.unwrap_or("HEAD"),
            round,
        };

        let outcome = run_job(&config, &spec, &paths, &mut log, &opts).await?;
        let events = EventLog::read(paths.events()).unwrap();

        Ok(Outcome {
            succeeded: outcome.passed(),
            job_id: paths.id,
            state: JobState::replay(&events),
            events: events.into_iter().map(|e| e.kind).collect(),
            log: paths.log(),
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
    /// Where the agent's captured output went.
    pub log: PathBuf,
}

impl Outcome {
    pub fn has(&self, predicate: impl Fn(&EventKind) -> bool) -> bool {
        self.events.iter().any(predicate)
    }
}
