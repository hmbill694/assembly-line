//! Getting a job's branch off the machine, as a pull request, once a round
//! on it has passed.

use crate::config::{Delivery, DeliveryMode};
use crate::git::{GroupKilledOnDrop, PinnedRef};
use crate::round::Verdict;
use std::path::Path;
use std::process::{Output, Stdio};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

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

/// The pull request targets a different branch from the one the job was cut
/// from, so its diff carries more than the job's work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseDiffers {
    pub base: String,
    pub base_ref: String,
}

impl std::fmt::Display for BaseDiffers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "this pull request will target '{}', but the job was cut from '{}' — review the \
             diff before merging, since it carries everything separating the two, not just this \
             job's work",
            self.base, self.base_ref
        )
    }
}

/// Whether a finished round's branch is worth a pull request, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryPlan {
    NothingToDeliver,
    OpenPullRequest {
        target: String,
        base_differs: Option<BaseDiffers>,
    },
}

/// What to deliver for a round with `verdict` that left the job's branch at
/// `branch_tip`. A passing branch carries work when its tip is not `base`'s
/// commit, which needs no git. Unset, `pull_request_target` is `base`.
///
/// A revise pins `base` again, so a branch that never got a commit also
/// differs from a base that has moved on, and `gh` is asked for that pull
/// request too. Into `base`, `gh` refuses it. Into another target, one that
/// lacks commits the old base has, it can open with only base commits and
/// none of the job's work; the [`BaseDiffers`] note is printed with it.
#[must_use]
pub fn delivery_plan(
    verdict: Verdict,
    branch_tip: &str,
    base: &PinnedRef,
    pull_request_target: Option<&str>,
) -> DeliveryPlan {
    match (verdict, branch_tip == base.sha) {
        (Verdict::Failed, _) | (Verdict::Passed, true) => DeliveryPlan::NothingToDeliver,
        (Verdict::Passed, false) => {
            let target = pull_request_target.unwrap_or(&base.name);
            DeliveryPlan::OpenPullRequest {
                target: target.to_string(),
                base_differs: (target != base.name).then(|| BaseDiffers {
                    base: target.to_string(),
                    base_ref: base.name.clone(),
                }),
            }
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

/// Ask for a pull request from `job_branch` into `base`, unless `cancel`
/// fires first. The round already pushed the branch, so nothing here can lose
/// work.
pub async fn deliver(
    repo: impl AsRef<Path>,
    delivery: &Delivery,
    job_branch: &str,
    base: &str,
    text: PullRequestText<'_>,
    cancel: &CancellationToken,
) -> Delivered {
    match delivery.mode {
        DeliveryMode::None => Delivered::Skipped("delivery mode is \"none\"".into()),
        DeliveryMode::Pr => open_pull_request(repo.as_ref(), job_branch, base, text, cancel)
            .await
            .unwrap_or_else(|| Delivered::Pushed {
                branch: job_branch.to_string(),
                because: "cancelled".to_string(),
            }),
    }
}

/// Ask `gh` for a pull request, unless the branch already has one. Never
/// fatal — the branch is already pushed. `None` once `cancel` has fired.
async fn open_pull_request(
    repo: &Path,
    job_branch: &str,
    base: &str,
    text: PullRequestText<'_>,
    cancel: &CancellationToken,
) -> Option<Delivered> {
    if let Some(url) = open_pull_request_of(repo, job_branch, cancel).await? {
        return Some(Delivered::AlreadyOpen { url });
    }
    let attempt = gh_unless_cancelled(
        repo,
        &[
            "pr", "create", "--base", base, "--head", job_branch, "--title", text.title, "--body",
            text.body,
        ],
        cancel,
    )
    .await?;

    Some(match attempt {
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
    })
}

/// The URL of `job_branch`'s open pull request, or `None` once `cancel` has
/// fired. An inner `None` covers no pull request, no `gh` and a `gh` that
/// refuses alike: each falls through to `gh pr create`, which reports its
/// own failure.
async fn open_pull_request_of(
    repo: &Path,
    job_branch: &str,
    cancel: &CancellationToken,
) -> Option<Option<String>> {
    let viewed = gh_unless_cancelled(
        repo,
        &[
            "pr",
            "view",
            job_branch,
            "--json",
            "url,state",
            "--jq",
            "select(.state == \"OPEN\") | .url",
        ],
        cancel,
    )
    .await?;
    Some(viewed.ok().and_then(|out| {
        let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (out.status.success() && !url.is_empty()).then_some(url)
    }))
}

/// `gh` with `args`, or `None` once `cancel` has fired.
///
/// `gh` leads a session of its own, as `git` does, so a cancel reaches it
/// only through `cancel`: were it in `run`'s process group, the signal that
/// cancels a local round would kill it too, and a killed `gh pr view` reads
/// as "no pull request open" — which goes on to `gh pr create`.
async fn gh_unless_cancelled(
    repo: &Path,
    args: &[&str],
    cancel: &CancellationToken,
) -> Option<std::io::Result<Output>> {
    if cancel.is_cancelled() {
        return None;
    }
    let child = match crate::exec::detach_from_terminal(&mut Command::new("gh"))
        .args(args)
        .current_dir(repo)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return Some(Err(e)),
    };
    let group = GroupKilledOnDrop::led_by(&child);
    tokio::select! {
        output = child.wait_with_output() => {
            group.disarm();
            Some(output)
        }
        () = cancel.cancelled() => None,
    }
}
