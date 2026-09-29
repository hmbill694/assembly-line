//! `submit`, before it reaches the daemon: which remote, which ref, what
//! prompt — resolved from the checkout without writing anything to it.

use crate::daemon::api::Submission;
use crate::git;
use crate::payload::remote_to_clone;
use crate::run::prompt_text;
use crate::workspace::DEFAULT_REMOTE;
use anyhow::anyhow;
use std::path::{Path, PathBuf};

/// What `submit` was asked for, as given on the command line.
#[derive(Debug, Clone, Default)]
pub struct SubmitRequest {
    pub prompt: Option<String>,
    pub prompt_file: Option<PathBuf>,
    /// A checkout, or a remote URL — as `run` takes it.
    pub repo: Option<String>,
    pub base_ref: Option<String>,
    pub provider: Option<String>,
    pub job: Option<u64>,
}

/// A job starts from the remote's copy of a ref. When the user's own copy
/// differs — usually unpushed commits — they should hear so, rather than
/// wonder where their work went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalRefDiffers {
    pub base_ref: String,
    pub remote: String,
    pub remote_sha: String,
}

impl std::fmt::Display for LocalRefDiffers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "note: your '{}' is not what '{}' has — the job starts from {}'s ({}); push first \
             if you meant yours",
            self.base_ref,
            self.remote,
            self.remote,
            &self.remote_sha[..12.min(self.remote_sha.len())]
        )
    }
}

/// A submission ready for the daemon, and what the user should hear first.
#[derive(Debug)]
pub struct PreparedSubmission {
    pub submission: Submission,
    pub notes: Vec<LocalRefDiffers>,
}

/// # Errors
///
/// When the prompt file cannot be read, there is no repository or remote,
/// or no ref can be settled on for a new job.
pub async fn prepare_submission(request: SubmitRequest) -> anyhow::Result<PreparedSubmission> {
    let prompt = prompt_text(request.prompt, request.prompt_file)?;
    let checkout = match request.repo {
        Some(named) if Path::new(&named).join(".git").exists() => PathBuf::from(named),
        Some(url) => {
            return for_remote_url(url, request.job, request.base_ref, prompt, request.provider);
        }
        None => crate::locate::checkout_named_or_enclosing(None)?,
    };
    let remote_url = remote_to_clone(&checkout, DEFAULT_REMOTE).await?;
    let base_ref =
        match (request.job, request.base_ref) {
            (Some(_), _) => None,
            (None, Some(named)) => Some(named),
            (None, None) => Some(git::current_branch(&checkout).await?.ok_or_else(|| {
                anyhow!("HEAD is detached — name the ref to start from with --ref")
            })?),
        };
    let notes = match &base_ref {
        None => Vec::new(),
        Some(base_ref) => local_ref_differs(&checkout, base_ref)
            .await
            .into_iter()
            .collect(),
    };
    Ok(PreparedSubmission {
        submission: Submission {
            remote_url,
            base_ref,
            job: request.job,
            prompt,
            provider: request.provider,
        },
        notes,
    })
}

/// A submission naming a remote URL: there is no checkout to take a branch
/// or a note from, so a new job must name its ref.
fn for_remote_url(
    remote_url: String,
    job: Option<u64>,
    base_ref: Option<String>,
    prompt: String,
    provider: Option<String>,
) -> anyhow::Result<PreparedSubmission> {
    let base_ref = match (job, base_ref) {
        (Some(_), _) => None,
        (None, Some(named)) => Some(named),
        (None, None) => {
            return Err(anyhow!(
                "a remote URL has no checked-out branch — name the ref to start from with --ref"
            ));
        }
    };
    Ok(PreparedSubmission {
        submission: Submission {
            remote_url,
            base_ref,
            job,
            prompt,
            provider,
        },
        notes: Vec::new(),
    })
}

/// A note only when both sides answered and disagree: a ref this checkout
/// lacks, or a remote that cannot be listed, is the daemon's to report.
async fn local_ref_differs(checkout: &Path, base_ref: &str) -> Option<LocalRefDiffers> {
    let local = git::sha_at_ref(checkout, base_ref).await.ok()?;
    let remote = git::sha_on_remote(checkout, DEFAULT_REMOTE, base_ref)
        .await
        .ok()??;
    (local != remote).then(|| LocalRefDiffers {
        base_ref: base_ref.to_string(),
        remote: DEFAULT_REMOTE.to_string(),
        remote_sha: remote,
    })
}
