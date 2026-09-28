//! Claiming a job's id: creating its branch on the remote before anything
//! else happens, so the claim and the work are the same object.

use crate::git;
use crate::job::JobId;
use crate::paths::job_id_past;
use std::path::Path;

/// How many ids a claim tries before giving up. Each loss means another
/// claimer took the id between our listing and our push; losing this many
/// times in a row means something is claiming far faster than we are.
const CLAIM_ATTEMPTS: usize = 16;

/// The next free job id on `remote`, claimed by creating its branch at
/// `base_sha`. `repo` must hold `base_sha`: the push sends it from there.
///
/// A loop, not a fold: each attempt is a listing and a push, and the first
/// push that creates its branch ends it.
///
/// # Errors
///
/// When the remote cannot be listed or pushed to, when a branch has taken
/// the last id there is, or when every attempt lost its race.
pub async fn claim_job(repo: &Path, remote: &str, base_sha: &str) -> anyhow::Result<JobId> {
    for _ in 0..CLAIM_ATTEMPTS {
        let taken = git::remote_branches_matching(repo, remote, JobId::BRANCH_PATTERN).await?;
        let id = job_id_past(&taken).ok_or_else(|| {
            anyhow::anyhow!(
                "a job branch has taken the last job id — delete {} from the remote",
                JobId::from(u64::MAX).branch_name()
            )
        })?;
        if git::create_branch_if_absent(repo, remote, base_sha, &id.branch_name()).await? {
            return Ok(id);
        }
    }
    Err(anyhow::anyhow!(
        "could not claim a job id on '{remote}': {CLAIM_ATTEMPTS} attempts in a row lost \
         to another claimer — try again"
    ))
}
