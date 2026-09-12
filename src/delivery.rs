//! Getting a finished job's work off the machine, as a pull request.
//!
//! Delivery is the last step of the job contract: a job's output is a branch,
//! and this is what makes that branch someone else's to look at.

use crate::config::{Delivery, DeliveryMode};
use crate::git;
use std::path::Path;
use tokio::process::Command;

/// What actually happened to the job's branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivered {
    /// Nothing was attempted, and this is why. Not a failure: a repository
    /// with no remote, or delivery turned off, is an ordinary local job.
    Skipped(String),
    /// Pushed, but no pull request was opened — `gh` is not installed, or it
    /// refused. The work is safe on the remote either way.
    Pushed {
        branch: String,
        because: String,
    },
    Opened {
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
        }
    }
}

/// Publish a finished job's branch according to `delivery`.
///
/// # Errors
///
/// Returns an error only if git itself fails — a push rejected because the
/// base moved, most often. A missing `gh` is reported as [`Delivered::Pushed`]
/// rather than an error: the branch reached the remote, which is the part that
/// matters.
pub async fn deliver(
    repo: impl AsRef<Path>,
    delivery: &Delivery,
    remote: &str,
    job_branch: &str,
    base: &str,
) -> anyhow::Result<Delivered> {
    let repo = repo.as_ref();

    match delivery.mode {
        DeliveryMode::None => Ok(Delivered::Skipped("delivery mode is \"none\"".into())),
        DeliveryMode::Pr if !git::remote_exists(repo, remote).await? => Ok(Delivered::Skipped(
            format!("the repository has no '{remote}' remote, so the branch stays local"),
        )),
        DeliveryMode::Pr => {
            git::push_branch(repo, remote, job_branch).await?;
            Ok(open_pull_request(repo, job_branch, base).await)
        }
    }
}

/// Ask `gh` for a pull request. Never fatal — the branch is already pushed, so
/// the worst case is that a human opens the PR themselves.
async fn open_pull_request(repo: &Path, job_branch: &str, base: &str) -> Delivered {
    let attempt = Command::new("gh")
        .args([
            "pr", "create", "--base", base, "--head", job_branch, "--fill",
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
