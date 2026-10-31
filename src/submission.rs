//! `submit`, before it reaches the daemon: which remote, which ref, what
//! prompt — resolved from the checkout without writing anything to it.

use crate::daemon::api::Submission;
use crate::git::{self, CommitId};
use crate::locate::NamedRepo;
use crate::remote::CHECKOUT_REMOTE;
use crate::run::prompt_text;
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
    pub remote_sha: CommitId,
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
            self.remote_sha.short()
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
    let repo = NamedRepo::named_or_enclosing(request.repo).await?;
    let base_ref = match request.job {
        Some(_) => None,
        None => Some(repo.ref_to_start_from(request.base_ref).await?),
    };
    let notes = match (&base_ref, &repo.checkout) {
        (Some(base_ref), Some(checkout)) => local_ref_differs(checkout, base_ref)
            .await
            .into_iter()
            .collect(),
        _ => Vec::new(),
    };
    Ok(PreparedSubmission {
        submission: Submission {
            remote_url: repo.remote.url().to_string(),
            base_ref,
            job: request.job,
            prompt,
            provider: request.provider,
        },
        notes,
    })
}

/// A note only when both sides answered and disagree: a ref this checkout
/// lacks, or a remote that cannot be listed, is the daemon's to report.
async fn local_ref_differs(checkout: &Path, base_ref: &str) -> Option<LocalRefDiffers> {
    let local = git::sha_at_ref(checkout, base_ref).await.ok()?;
    let remote = git::sha_on_remote(checkout, CHECKOUT_REMOTE, base_ref)
        .await
        .ok()??;
    (local != remote).then(|| LocalRefDiffers {
        base_ref: base_ref.to_string(),
        remote: CHECKOUT_REMOTE.to_string(),
        remote_sha: remote,
    })
}
