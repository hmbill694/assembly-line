//! What a repository declares about how the factory builds it.
//!
//! There is no user-authored graph file any more. A job's prompt arrives on
//! the command line; everything else comes from [`REPO_CONFIG_PATH`], read
//! from the ref the job is cut from.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

/// Where a repository declares its factory settings.
pub const REPO_CONFIG_PATH: &str = ".assembly/config.toml";

/// A setting that makes a repository unrunnable as it stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    UnknownProvider(String),
    NoProviderDeclared,
    UnparseableMaxDuration(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownProvider(name) => write!(
                f,
                "provider '{name}' is not declared in [providers] — add a block for it"
            ),
            Self::NoProviderDeclared => write!(
                f,
                "no provider: set `provider = \"...\"` and declare it under [providers]"
            ),
            Self::UnparseableMaxDuration(value) => {
                write!(f, "max_duration '{value}' is not a duration like \"20m\"")
            }
        }
    }
}

// So a `ConfigError` can travel as an `anyhow::Error` from the one place a
// job's plan cannot be built — without flattening it to a bare string first.
impl std::error::Error for ConfigError {}

/// A setting worth flagging but not worth refusing to run over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Warning {
    NoVerify,
}

impl std::fmt::Display for Warning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoVerify => write!(
                f,
                "no `verify` — nothing will check a job's output before it is delivered"
            ),
        }
    }
}

/// One repository's factory settings, as the repository itself declares them.
///
/// Read from a ref, never from a checkout — see [`RepoConfig::from_ref`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoConfig {
    /// Which provider a job uses unless the command line names another.
    pub provider: Option<String>,
    /// The command that decides whether a job's work is correct.
    pub verify: Option<String>,
    /// What work lands on. Defaults to the branch the job was cut from.
    pub base: Option<String>,
    /// Wall-clock cap on one agent invocation.
    pub max_duration: Option<String>,
    /// Untracked files a job's checkout needs — `.env`, local settings.
    #[serde(default)]
    pub copy: Vec<String>,
    #[serde(default)]
    pub providers: BTreeMap<String, Provider>,
    #[serde(default)]
    pub delivery: Delivery,
}

impl RepoConfig {
    /// Read a repository's configuration as of `git_ref`.
    ///
    /// # Errors
    ///
    /// Returns an error if the ref does not carry [`REPO_CONFIG_PATH`] — a
    /// repository that has not opted in — or if the file is not valid TOML.
    pub async fn from_ref(repo: &Path, git_ref: &str) -> anyhow::Result<RepoConfig> {
        let src = crate::git::file_at_ref(repo, git_ref, REPO_CONFIG_PATH)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "{git_ref} carries no {REPO_CONFIG_PATH} — this repository is not opted in"
                )
            })?;

        Self::parse(&src)
    }

    /// Parse configuration from TOML text, without touching git.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed syntax or an unknown key — the latter
    /// because every config struct denies unknown fields, so a typo is
    /// reported rather than silently defaulted.
    pub fn parse(src: &str) -> anyhow::Result<RepoConfig> {
        toml::from_str(src).map_err(|e| anyhow::anyhow!("parsing {REPO_CONFIG_PATH}: {e}"))
    }

    /// Every reason this repository cannot run a job with `provider`, so all
    /// of them are fixed in one pass rather than one per run.
    ///
    /// `provider` is what the job will actually use — the command line's
    /// choice when it named one, otherwise [`RepoConfig::provider`].
    #[must_use]
    pub fn problems(&self, provider: &str) -> Vec<ConfigError> {
        let undeclared = match (provider.is_empty(), self.providers.contains_key(provider)) {
            (true, _) => Some(ConfigError::NoProviderDeclared),
            (false, false) => Some(ConfigError::UnknownProvider(provider.to_string())),
            (false, true) => None,
        };

        undeclared
            .into_iter()
            .chain(self.unparseable_max_duration())
            .collect()
    }

    fn unparseable_max_duration(&self) -> Option<ConfigError> {
        let value = self.max_duration.as_deref()?;
        parse_duration(value)
            .is_err()
            .then(|| ConfigError::UnparseableMaxDuration(value.to_string()))
    }

    /// Settings worth telling the user about, none of which stop a job.
    #[must_use]
    pub fn warnings(&self) -> Vec<Warning> {
        self.verify
            .is_none()
            .then_some(Warning::NoVerify)
            .into_iter()
            .collect()
    }
}

/// What becomes of a job's branch once the job finishes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeliveryMode {
    /// Push the branch and open a pull request against `base`.
    #[default]
    Pr,
    /// Leave the branch where it is.
    None,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delivery {
    #[serde(default)]
    pub mode: DeliveryMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provider {
    pub cmd: String,
    #[serde(default)]
    pub args: Vec<String>,
}

/// Parse a human-written duration such as `"20m"` or `"1h 30m"`.
///
/// # Errors
///
/// Returns an error if the text is not a recognisable duration.
pub fn parse_duration(s: &str) -> anyhow::Result<Duration> {
    humantime::parse_duration(s).map_err(|e| anyhow::anyhow!("invalid duration {s:?}: {e}"))
}
