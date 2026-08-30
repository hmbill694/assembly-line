use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskKind {
    Shell,
    Agent,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Supervise {
    #[default]
    None,
    Pre,
    OnComplete,
    Both,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OnFailure {
    #[default]
    Skip,
    Abort,
    Continue,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub id: String,
    pub kind: TaskKind,
    #[serde(default)]
    pub needs: Vec<String>,

    // shell
    pub run: Option<String>,

    // agent — parsed in M1, executed in M2
    /// Inline prompt text. Mutually exclusive with `prompt_file`.
    pub prompt: Option<String>,
    /// Path to a file holding the prompt, resolved relative to the graph file.
    /// `load_graph` reads it into `prompt`, so nothing downstream has to know
    /// which form was used.
    pub prompt_file: Option<PathBuf>,
    pub provider: Option<String>,
    pub output_file: Option<String>,

    // shared
    pub verify: Option<String>,
    pub resource: Option<String>,
    #[serde(default)]
    pub copy: Vec<String>,
    #[serde(default)]
    pub supervise: Supervise,
    #[serde(default)]
    pub retries: u32,
    pub max_duration: Option<String>,
    pub max_cost_usd: Option<f64>,
    #[serde(default)]
    pub on_failure: OnFailure,
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
