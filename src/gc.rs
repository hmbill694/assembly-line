//! Collecting what jobs leave behind.
//!
//! A job discards its own checkout as it finishes, so what is found here is
//! whatever a job that died mid-round orphaned. The policy: a job's worktrees
//! are wanted exactly as long as the job is, and `--older-than` exists only
//! for the leftovers of jobs whose state was never cleaned up.
//!
//! Deciding and doing are separate: [`collectable`] reports what could go and
//! why, which is what `--dry-run` prints, and [`remove`] is the only part that
//! deletes anything.

use crate::git;
use crate::paths;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A job's worktree directory that nothing needs any more.
#[derive(Debug, Clone)]
pub struct StaleWorktree {
    pub path: PathBuf,
    /// Why it is collectable, in the words `gc` prints.
    pub because: String,
}

/// One repository's leftovers, paired with the repository itself so its
/// worktree list can be pruned after the directories go.
#[derive(Debug, Clone)]
pub struct RepositoryLeftovers {
    /// `None` when the marker naming the repository is missing, which by
    /// itself makes everything under it collectable.
    pub repo: Option<PathBuf>,
    pub stale: Vec<StaleWorktree>,
}

fn job_directories(dir: &Path) -> Vec<(u64, PathBuf)> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let id = entry.file_name().to_str()?.parse::<u64>().ok()?;
            Some((id, entry.path()))
        })
        .collect()
}

fn subdirectories(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect()
}

fn idle_time(path: &Path) -> Option<Duration> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|written| written.elapsed().ok())
}

/// Why a job's worktree directory is collectable, or `None` while the job it
/// belongs to still exists.
#[must_use]
pub fn reason_to_collect(
    repo: Option<&Path>,
    job_id: u64,
    path: &Path,
    keep_for: Option<Duration>,
) -> Option<String> {
    // Age only collects when the user asked for it: a worktree whose job still
    // exists is still wanted however old it is.
    let too_old = || {
        let keep_for = keep_for?;
        let idle = idle_time(path)?;
        (idle > keep_for).then(|| {
            format!(
                "untouched for {}",
                humantime::format_duration(Duration::from_secs(idle.as_secs()))
            )
        })
    };

    match repo {
        None => Some("its repository is unknown".to_string()),
        Some(repo) if !repo.exists() => Some(format!("{} no longer exists", repo.display())),
        Some(repo) if !paths::jobs_root(repo).join(job_id.to_string()).is_dir() => {
            Some(format!("job {job_id} has no state directory"))
        }
        Some(_) => too_old(),
    }
}

/// Every collectable worktree directory, grouped by the repository it belongs
/// to. Reports only — nothing is removed.
#[must_use]
pub fn collectable(keep_for: Option<Duration>) -> Vec<RepositoryLeftovers> {
    let Some(root) = paths::worktrees_root() else {
        return Vec::new();
    };

    subdirectories(&root)
        .into_iter()
        .map(|per_repo| {
            let repo = paths::repository_owning_worktrees(&per_repo);
            RepositoryLeftovers {
                stale: job_directories(&per_repo)
                    .into_iter()
                    .filter_map(|(job_id, path)| {
                        reason_to_collect(repo.as_deref(), job_id, &path, keep_for)
                            .map(|because| StaleWorktree { path, because })
                    })
                    .collect(),
                repo,
            }
        })
        .collect()
}

#[must_use]
pub fn total(found: &[RepositoryLeftovers]) -> usize {
    found.iter().map(|leftovers| leftovers.stale.len()).sum()
}

/// What a collection actually managed to do.
#[derive(Debug, Default)]
pub struct Removed {
    pub directories: usize,
    /// Non-fatal problems, phrased for the user. A worktree that resists
    /// removal is worth saying so about, not worth failing the command over.
    pub warnings: Vec<String>,
}

/// Delete the directories in `found`, then [`git::prune_worktrees`] each
/// repository.
pub async fn remove(found: &[RepositoryLeftovers]) -> Removed {
    let mut removed = Removed::default();

    // A loop, not an iterator chain: each repository's prune is awaited, and
    // std has no async fold to thread the accumulator through.
    for leftovers in found {
        let deleted = delete_directories(&leftovers.stale);
        removed.directories += deleted.directories;
        removed.warnings.extend(deleted.warnings);
        removed.warnings.extend(prune_warning(leftovers).await);
    }

    removed
}

/// Delete one repository's stale directories, keeping the reason for each one
/// that would not go.
fn delete_directories(stale: &[StaleWorktree]) -> Removed {
    let warnings: Vec<String> = stale
        .iter()
        .filter_map(|entry| {
            std::fs::remove_dir_all(&entry.path)
                .err()
                .map(|e| format!("could not remove {}: {e}", entry.path.display()))
        })
        .collect();

    Removed {
        // Every entry either went or left a warning, so this cannot underflow.
        directories: stale.len() - warnings.len(),
        warnings,
    }
}

/// Why pruning a repository's worktree list failed, or `None` when it worked,
/// when nothing was removed to make it worth doing, or when the repository is
/// unknown or gone.
async fn prune_warning(leftovers: &RepositoryLeftovers) -> Option<String> {
    let repo = leftovers
        .repo
        .as_deref()
        .filter(|repo| !leftovers.stale.is_empty() && repo.exists())?;

    git::prune_worktrees(repo)
        .await
        .err()
        .map(|e| format!("could not prune worktrees in {}: {e}", repo.display()))
}
