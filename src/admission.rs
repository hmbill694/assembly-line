//! Whether a round may run, decided once, before anything is claimed for it:
//! the repository's config at the round's pinned base, checked into the
//! settings the round runs under.

use crate::config::{ConfigError, JobSettings, REPO_CONFIG_PATH, RepoConfig};
use crate::git::PinnedRef;
use std::path::Path;

/// A round that may run: the base its config was read at, and the settings
/// that config gives it. Only [`admit`] makes one and nothing changes it,
/// and claiming a job's id takes one, so a job is claimed at the commit its
/// settings came from.
#[derive(Debug, Clone)]
pub struct Admitted {
    base: PinnedRef,
    settings: JobSettings,
}

impl Admitted {
    #[must_use]
    pub fn base(&self) -> &PinnedRef {
        &self.base
    }

    #[must_use]
    pub fn settings(&self) -> &JobSettings {
        &self.settings
    }

    #[must_use]
    pub fn into_base_and_settings(self) -> (PinnedRef, JobSettings) {
        (self.base, self.settings)
    }
}

/// Why a round may not run.
#[derive(Debug)]
pub enum AdmissionRefused {
    /// The base carries no config, or one that is not valid TOML.
    Unreadable(anyhow::Error),
    NotRunnable(Vec<ConfigError>),
}

impl AdmissionRefused {
    /// Every reason, one per line, to report ahead of the refusal itself.
    #[must_use]
    pub fn itemized_reasons(&self) -> Vec<String> {
        match self {
            Self::Unreadable(_) => Vec::new(),
            Self::NotRunnable(errors) => errors.iter().map(ToString::to_string).collect(),
        }
    }

    /// The refusal and every reason for it, as one line.
    #[must_use]
    pub fn to_failure_reason(&self) -> String {
        match self.itemized_reasons().as_slice() {
            [] => self.to_string(),
            reasons => format!("{self}: {}", reasons.join("; ")),
        }
    }
}

impl std::fmt::Display for AdmissionRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreadable(e) => write!(f, "{e}"),
            Self::NotRunnable(_) => write!(f, "{REPO_CONFIG_PATH} is not runnable"),
        }
    }
}

/// Admit a round on `base` that names `provider_named`, reading the config
/// at `base` from `repo`, which must hold that commit.
///
/// # Errors
///
/// An [`AdmissionRefused`] holding every reason the round may not run.
pub async fn admit(
    repo: &Path,
    base: PinnedRef,
    provider_named: Option<&str>,
) -> Result<Admitted, AdmissionRefused> {
    let config = RepoConfig::from_ref(repo, base.sha.as_str())
        .await
        .map_err(AdmissionRefused::Unreadable)?;
    let settings = config
        .settings_for(provider_named)
        .map_err(AdmissionRefused::NotRunnable)?;
    Ok(Admitted { base, settings })
}
