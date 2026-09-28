//! Getting a finished job's branch off the machine, as a pull request.

use crate::config::{Delivery, DeliveryMode};
use std::path::Path;
use tokio::process::Command;

/// What actually happened to the job's branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivered {
    /// Nothing was attempted, and this is why: delivery is turned off.
    Skipped(String),
    /// The round pushed the branch, but no pull request was opened — `gh` is
    /// not installed, or it refused.
    Pushed {
        branch: String,
        because: String,
    },
    Opened {
        url: String,
    },
    /// The branch already had a pull request — a revise, whose new commits
    /// land on it by themselves.
    AlreadyOpen {
        url: String,
    },
}

impl std::fmt::Display for Delivered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Skipped(why) => write!(f, "not delivered: {why}"),
            Self::Pushed { branch, because } => {
                write!(f, "pushed {branch} — no pull request opened: {because}")
            }
            Self::Opened { url } => write!(f, "opened {url}"),
            Self::AlreadyOpen { url } => write!(f, "updated {url}"),
        }
    }
}

/// What a pull request says about itself.
///
/// Given explicitly rather than left to `gh pr create --fill`, which reads
/// the branch's commits from the local repository — and a job's branch
/// exists only on the remote.
#[derive(Debug, Clone, Copy)]
pub struct PullRequestText<'a> {
    pub title: &'a str,
    pub body: &'a str,
}

/// Ask for a pull request from `job_branch` into `base`. The job already
/// pushed the branch, so nothing here can lose work.
pub async fn deliver(
    repo: impl AsRef<Path>,
    delivery: &Delivery,
    job_branch: &str,
    base: &str,
    text: PullRequestText<'_>,
) -> Delivered {
    match delivery.mode {
        DeliveryMode::None => Delivered::Skipped("delivery mode is \"none\"".into()),
        DeliveryMode::Pr => open_pull_request(repo.as_ref(), job_branch, base, text).await,
    }
}

/// Ask `gh` for a pull request, unless the branch already has one. Never
/// fatal — the branch is already pushed.
async fn open_pull_request(
    repo: &Path,
    job_branch: &str,
    base: &str,
    text: PullRequestText<'_>,
) -> Delivered {
    if let Some(url) = open_pull_request_of(repo, job_branch).await {
        return Delivered::AlreadyOpen { url };
    }
    let attempt = Command::new("gh")
        .args([
            "pr", "create", "--base", base, "--head", job_branch, "--title", text.title, "--body",
            text.body,
        ])
        .current_dir(repo)
        .output()
        .await;

    match attempt {
        Err(e) => Delivered::Pushed {
            branch: job_branch.to_string(),
            because: format!("could not run `gh`: {e}"),
        },
        Ok(out) if !out.status.success() => Delivered::Pushed {
            branch: job_branch.to_string(),
            because: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        },
        Ok(out) => Delivered::Opened {
            url: String::from_utf8_lossy(&out.stdout).trim().to_string(),
        },
    }
}

/// The URL of `job_branch`'s open pull request. `None` covers no pull
/// request, no `gh` and a `gh` that refuses alike: each falls through to
/// `gh pr create`, which reports its own failure.
async fn open_pull_request_of(repo: &Path, job_branch: &str) -> Option<String> {
    let out = Command::new("gh")
        .args([
            "pr",
            "view",
            job_branch,
            "--json",
            "url,state",
            "--jq",
            "select(.state == \"OPEN\") | .url",
        ])
        .current_dir(repo)
        .output()
        .await
        .ok()?;
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !url.is_empty()).then_some(url)
}
