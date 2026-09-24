//! Getting a finished job's branch off the machine, as a pull request.

use crate::config::{Delivery, DeliveryMode};
use std::path::Path;
use tokio::process::Command;

/// What actually happened to the job's branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivered {
    /// Nothing was attempted, and this is why: delivery is turned off.
    Skipped(String),
    /// The job pushed the branch, but no pull request was opened — `gh` is
    /// not installed, or it refused.
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

/// Ask for a pull request from `job_branch` into `base`. The job already
/// pushed the branch, so nothing here can lose work.
pub async fn deliver(
    repo: impl AsRef<Path>,
    delivery: &Delivery,
    job_branch: &str,
    base: &str,
) -> Delivered {
    match delivery.mode {
        DeliveryMode::None => Delivered::Skipped("delivery mode is \"none\"".into()),
        DeliveryMode::Pr => open_pull_request(repo.as_ref(), job_branch, base).await,
    }
}

/// Ask `gh` for a pull request. Never fatal — the branch is already pushed.
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
