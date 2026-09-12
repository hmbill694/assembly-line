use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    DuplicateId(String),
    InvalidId(String),
    ReservedId(String),
    AgentMissingPrompt(String),
    UnknownProvider { task: String, provider: String },
    InvalidDuration { task: String, value: String },
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateId(id) => write!(f, "duplicate task id '{id}'"),
            Self::InvalidId(id) => write!(
                f,
                "invalid task id '{id}': ids must match [A-Za-z0-9_-]+ (they become filenames and branch names)"
            ),
            Self::ReservedId(id) => write!(
                f,
                "task id '{id}' is reserved: ids may not start with '_', which assembly-line keeps for itself"
            ),
            Self::AgentMissingPrompt(id) => {
                write!(f, "agent task '{id}' has no `prompt` or `prompt_file`")
            }
            Self::UnknownProvider { task, provider } => {
                write!(f, "task '{task}' uses undefined provider '{provider}'")
            }
            Self::InvalidDuration { task, value } => {
                write!(f, "task '{task}' has invalid max_duration '{value}'")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Warning {
    AgentWithoutVerify(String),
    CostCapWithoutAdapter { task: String, provider: String },
}

impl std::fmt::Display for Warning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AgentWithoutVerify(id) => write!(
                f,
                "agent task '{id}' declares no `verify` — nothing will check its output"
            ),
            Self::CostCapWithoutAdapter { task, provider } => write!(
                f,
                "task '{task}' sets max_cost_usd but provider '{provider}' reports no cost — the cap will not apply"
            ),
        }
    }
}

#[derive(Debug)]
pub struct Validation {
    pub errors: Vec<ValidationError>,
    pub warnings: Vec<Warning>,
}

fn id_is_valid(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Ids that are unusable as filenames, ids assembly-line has claimed for
/// itself, and ids declared more than once.
fn id_naming_errors(tasks: &[Task]) -> impl Iterator<Item = ValidationError> + '_ {
    let invalid = tasks
        .iter()
        .filter(|t| !id_is_valid(&t.id))
        .map(|t| ValidationError::InvalidId(t.id.clone()));

    let duplicated = tasks
        .iter()
        .enumerate()
        .filter(|(i, t)| tasks[..*i].iter().any(|prior| prior.id == t.id))
        .map(|(_, t)| ValidationError::DuplicateId(t.id.clone()));

    // Ids become directory names beside assembly-line's own bookkeeping
    // entries, whose names start with `_`. Reserving the whole prefix keeps
    // that space ours.
    let reserved = tasks
        .iter()
        .filter(|t| t.id.starts_with('_'))
        .map(|t| ValidationError::ReservedId(t.id.clone()));

    invalid.chain(duplicated).chain(reserved)
}

/// A task must carry a prompt to be runnable at all. It may supply it inline
/// or by file; `load_graph` folds the latter into the former, so either
/// satisfies this check.
fn missing_required_field(t: &Task) -> Option<ValidationError> {
    match (&t.prompt, &t.prompt_file) {
        (None, None) => Some(ValidationError::AgentMissingPrompt(t.id.clone())),
        _ => None,
    }
}

fn unparseable_max_duration(t: &Task) -> Option<ValidationError> {
    match &t.max_duration {
        Some(d) if parse_duration(d).is_err() => Some(ValidationError::InvalidDuration {
            task: t.id.clone(),
            value: d.clone(),
        }),
        _ => None,
    }
}

/// A task's provider must exist among the graph's declared providers.
fn unknown_provider(graph: &Graph, t: &Task) -> Option<ValidationError> {
    match &t.provider {
        Some(name) if !graph.providers.contains_key(name) => {
            Some(ValidationError::UnknownProvider {
                task: t.id.clone(),
                provider: name.clone(),
            })
        }
        _ => None,
    }
}

/// Checks against a task's declared provider that are worth flagging but not
/// rejecting outright: no oversight at all, or a cost cap the provider cannot
/// honour.
fn agent_checks(graph: &Graph, t: &Task) -> Vec<Warning> {
    let Some(provider) = t
        .provider
        .as_ref()
        .and_then(|name| graph.providers.get(name))
    else {
        return Vec::new();
    };

    let unverified = t
        .verify
        .is_none()
        .then(|| Warning::AgentWithoutVerify(t.id.clone()));
    let uncapped = (t.max_cost_usd.is_some() && provider.adapter.is_none()).then(|| {
        Warning::CostCapWithoutAdapter {
            task: t.id.clone(),
            provider: t.provider.clone().unwrap_or_default(),
        }
    });

    unverified.into_iter().chain(uncapped).collect()
}

/// Every problem in a graph file, so a user fixes all of them in one pass
/// rather than one per run.
#[must_use]
pub fn validate(graph: &Graph) -> Validation {
    Validation {
        errors: id_naming_errors(&graph.tasks)
            .chain(graph.tasks.iter().filter_map(missing_required_field))
            .chain(
                graph
                    .tasks
                    .iter()
                    .filter_map(|t| unknown_provider(graph, t)),
            )
            .chain(graph.tasks.iter().filter_map(unparseable_max_duration))
            .collect(),
        warnings: graph
            .tasks
            .iter()
            .flat_map(|t| agent_checks(graph, t))
            .collect(),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Graph {
    #[serde(default)]
    pub workspace: Workspace,
    #[serde(default)]
    pub delivery: Delivery,
    #[serde(default)]
    pub providers: BTreeMap<String, Provider>,
    #[serde(rename = "task", default)]
    pub tasks: Vec<Task>,
    #[serde(rename = "hook", default)]
    pub hooks: Vec<Hook>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    #[serde(default)]
    pub copy: Vec<String>,
}

/// What becomes of the run branch once the graph finishes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeliveryMode {
    /// Push the run branch and open a pull request against `base`.
    #[default]
    Pr,
    /// Fast-forward `base` on the remote to the run branch. For work you
    /// trust to land unreviewed.
    Push,
    /// Leave the branch where it is.
    None,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delivery {
    #[serde(default)]
    pub mode: DeliveryMode,
    /// What the work lands on. Defaults to the branch the run started from —
    /// never an assumed `main`.
    pub base: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provider {
    pub cmd: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Optional enrichment wrapper emitting assembly-line NDJSON. Unused in M1.
    pub adapter: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hook {
    pub on: String,
    pub run: String,
    pub when: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub id: String,

    /// Inline prompt text. Mutually exclusive with `prompt_file`.
    pub prompt: Option<String>,
    /// Path to a file holding the prompt, resolved relative to the graph file.
    /// `load_graph` reads it into `prompt`, so nothing downstream has to know
    /// which form was used.
    pub prompt_file: Option<PathBuf>,
    pub provider: Option<String>,
    pub output_file: Option<String>,

    pub verify: Option<String>,
    #[serde(default)]
    pub copy: Vec<String>,
    #[serde(default)]
    pub retries: u32,
    pub max_duration: Option<String>,
    pub max_cost_usd: Option<f64>,
}

/// Parse a graph from TOML text, without touching the filesystem.
///
/// # Errors
///
/// Returns a TOML error for malformed syntax, a missing required field, or an
/// unknown field — the last because every config struct denies unknown keys,
/// so a typo is reported rather than silently defaulted.
pub fn parse_graph(src: &str) -> Result<Graph, toml::de::Error> {
    toml::from_str(src)
}

/// Read and parse a graph file, resolving any `prompt_file` references.
///
/// # Errors
///
/// Returns an error if the file cannot be read, the TOML is invalid, or a
/// task's `prompt_file` is missing or conflicts with an inline `prompt`.
pub fn load_graph(path: &Path) -> anyhow::Result<Graph> {
    let src = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;
    let graph =
        parse_graph(&src).map_err(|e| anyhow::anyhow!("parsing {}: {e}", path.display()))?;

    inline_prompt_files(graph, path.parent().unwrap_or_else(|| Path::new(".")))
}

/// Replace every `prompt_file` with the file's contents, so the rest of the
/// system only ever deals with `prompt`.
///
/// Paths resolve relative to the graph file's own directory, which makes a
/// graph plus its prompts a self-contained, movable unit.
///
/// # Errors
///
/// Returns an error if a task sets both `prompt` and `prompt_file`, or if a
/// referenced prompt file cannot be read. Both fail here, before a run
/// directory is allocated, so a typo costs nothing.
pub fn inline_prompt_files(graph: Graph, graph_dir: &Path) -> anyhow::Result<Graph> {
    let tasks = graph
        .tasks
        .into_iter()
        .map(|task| match (&task.prompt, &task.prompt_file) {
            (Some(_), Some(file)) => Err(anyhow::anyhow!(
                "task '{}' sets both `prompt` and `prompt_file` ({}) — use one",
                task.id,
                file.display()
            )),
            (None, Some(file)) => {
                let full = graph_dir.join(file);
                let text = std::fs::read_to_string(&full).map_err(|e| {
                    anyhow::anyhow!(
                        "task '{}': reading prompt_file {}: {e}",
                        task.id,
                        full.display()
                    )
                })?;
                Ok(Task {
                    prompt: Some(text),
                    ..task
                })
            }
            _ => Ok(task),
        })
        .collect::<anyhow::Result<Vec<Task>>>()?;

    Ok(Graph { tasks, ..graph })
}

/// Parse a human-written duration such as `"20m"` or `"1h 30m"`.
///
/// # Errors
///
/// Returns an error if the text is not a recognisable duration.
pub fn parse_duration(s: &str) -> anyhow::Result<Duration> {
    humantime::parse_duration(s).map_err(|e| anyhow::anyhow!("invalid duration {s:?}: {e}"))
}
