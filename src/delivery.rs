//! Getting a finished run's work off the machine — as a pull request, or
//! straight onto the base branch.
//!
//! Delivery is the last step of the job contract: a run's output is a branch,
//! and this is what makes that branch someone else's to look at.

use crate::config::{Delivery, DeliveryMode};
use crate::git;
use std::path::Path;
use tokio::process::Command;

/// What actually happened to the run branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivered {
    /// Nothing was attempted, and this is why. Not a failure: a repository
    /// with no remote, or delivery turned off, is an ordinary local run.
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
    /// The base branch on the remote now carries the run's work.
    LandedOn {
        base: String,
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
            Self::LandedOn { base } => write!(f, "pushed onto {base}"),
        }
    }
}

/// Publish a finished run's branch according to `delivery`.
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
    run_branch: &str,
    base: &str,
) -> anyhow::Result<Delivered> {
    let repo = repo.as_ref();

    if delivery.mode == DeliveryMode::None {
        return Ok(Delivered::Skipped("delivery mode is \"none\"".into()));
    }
    if !git::remote_exists(repo, remote).await? {
        return Ok(Delivered::Skipped(format!(
            "the repository has no '{remote}' remote, so the branch stays local"
        )));
    }

    git::push_branch(repo, remote, run_branch).await?;

    match delivery.mode {
        // Fast-forwarding the base on the remote. A non-fast-forward is
        // rejected by git, which is the correct outcome: the base moved and
        // this work has not seen it.
        DeliveryMode::Push => {
            git::push_refspec(repo, remote, &format!("{run_branch}:{base}")).await?;
            Ok(Delivered::LandedOn {
                base: base.to_string(),
            })
        }
        DeliveryMode::Pr => Ok(open_pull_request(repo, run_branch, base).await),
        DeliveryMode::None => unreachable!("returned above"),
    }
}

/// Ask `gh` for a pull request. Never fatal — the branch is already pushed, so
/// the worst case is that a human opens the PR themselves.
async fn open_pull_request(repo: &Path, run_branch: &str, base: &str) -> Delivered {
    let attempt = Command::new("gh")
        .args([
            "pr", "create", "--base", base, "--head", run_branch, "--fill",
        ])
        .current_dir(repo)
        .output()
        .await;

    match attempt {
        Err(e) => Delivered::Pushed {
            branch: run_branch.to_string(),
            because: format!("could not run `gh`: {e}"),
        },
        Ok(out) if !out.status.success() => Delivered::Pushed {
            branch: run_branch.to_string(),
            because: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        },
        Ok(out) => Delivered::Opened {
            url: String::from_utf8_lossy(&out.stdout).trim().to_string(),
        },
    }
}
