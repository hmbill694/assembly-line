use crate::config::{Graph, Supervise, Task, TaskKind};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    DuplicateId(String),
    InvalidId(String),
    SelfDep(String),
    UnknownDep { task: String, dep: String },
    Cycle(Vec<String>),
    ShellMissingRun(String),
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
            Self::SelfDep(id) => write!(f, "task '{id}' depends on itself"),
            Self::UnknownDep { task, dep } => {
                write!(f, "task '{task}' needs '{dep}', which does not exist")
            }
            Self::Cycle(path) => write!(f, "dependency cycle: {}", path.join(" -> ")),
            Self::ShellMissingRun(id) => write!(f, "shell task '{id}' has no `run`"),
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
    UnsupervisedAgentWithoutVerify(String),
    CostCapWithoutAdapter { task: String, provider: String },
}

impl std::fmt::Display for Warning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupervisedAgentWithoutVerify(id) => write!(
                f,
                "agent task '{id}' is unsupervised and declares no `verify` — nothing will check its output, and merge conflicts cannot be auto-resolved"
            ),
            Self::CostCapWithoutAdapter { task, provider } => write!(
                f,
                "task '{task}' sets max_cost_usd but provider '{provider}' reports no cost — the cap will not apply"
            ),
        }
    }
}

fn id_is_valid(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Ids in declaration order, first occurrence winning.
fn ids_in_declaration_order(tasks: &[Task]) -> Vec<String> {
    tasks.iter().fold(Vec::new(), |mut acc, t| {
        match acc.iter().any(|id| id == &t.id) {
            true => acc,
            false => {
                acc.push(t.id.clone());
                acc
            }
        }
    })
}

/// Ids that are unusable as filenames, and ids declared more than once.
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

    invalid.chain(duplicated)
}

/// A task must carry the field its kind needs to be runnable at all. An agent
/// may supply its prompt inline or by file; `load_graph` folds the latter into
/// the former, so either satisfies this check.
fn missing_required_field(t: &Task) -> Option<ValidationError> {
    match (t.kind, &t.run, &t.prompt, &t.prompt_file) {
        (TaskKind::Shell, None, _, _) => Some(ValidationError::ShellMissingRun(t.id.clone())),
        (TaskKind::Agent, _, None, None) => Some(ValidationError::AgentMissingPrompt(t.id.clone())),
        _ => None,
    }
}

/// Dependencies that point at nothing, or back at the task itself.
fn unresolvable_dependencies<'a>(
    tasks: &'a [Task],
    known: &'a BTreeSet<String>,
) -> impl Iterator<Item = ValidationError> + 'a {
    tasks.iter().flat_map(move |t| {
        t.needs.iter().filter_map(move |dep| match dep {
            d if d == &t.id => Some(ValidationError::SelfDep(t.id.clone())),
            d if !known.contains(d) => Some(ValidationError::UnknownDep {
                task: t.id.clone(),
                dep: dep.clone(),
            }),
            _ => None,
        })
    })
}

#[derive(Debug, Clone)]
pub struct Dag {
    ids: Vec<String>,
    needs: BTreeMap<String, Vec<String>>,
    dependents: BTreeMap<String, Vec<String>>,
}

impl Dag {
    pub fn build(tasks: &[Task]) -> Result<Dag, Vec<ValidationError>> {
        let ids = ids_in_declaration_order(tasks);
        let known: BTreeSet<String> = ids.iter().cloned().collect();

        // Non-structural problems are collected up front; the cycle check
        // needs a built Dag, so it is chained on afterwards.
        let per_task_errors: Vec<ValidationError> = id_naming_errors(tasks)
            .chain(tasks.iter().filter_map(missing_required_field))
            .chain(unresolvable_dependencies(tasks, &known))
            .collect();

        // `needs` keeps only edges that resolve, so the adjacency maps stay
        // well-formed even while errors are still being collected.
        let needs: BTreeMap<String, Vec<String>> = ids
            .iter()
            .map(|id| {
                let deps = tasks
                    .iter()
                    .find(|t| &t.id == id)
                    .into_iter()
                    .flat_map(|t| t.needs.iter())
                    .filter(|d| *d != id && known.contains(*d))
                    .cloned()
                    .collect();
                (id.clone(), deps)
            })
            .collect();

        let dependents: BTreeMap<String, Vec<String>> = ids
            .iter()
            .map(|id| {
                let children = ids
                    .iter()
                    .filter(|other| needs[*other].contains(id))
                    .cloned()
                    .collect();
                (id.clone(), children)
            })
            .collect();

        let dag = Dag {
            ids,
            needs,
            dependents,
        };

        let errors: Vec<ValidationError> = per_task_errors
            .into_iter()
            .chain(dag.find_cycle().map(ValidationError::Cycle))
            .collect();

        match errors.is_empty() {
            true => Ok(dag),
            false => Err(errors),
        }
    }

    pub fn ids(&self) -> &[String] {
        &self.ids
    }

    pub fn needs(&self, id: &str) -> &[String] {
        self.needs.get(id).map(Vec::as_slice).unwrap_or_default()
    }

    pub fn dependents(&self, id: &str) -> &[String] {
        self.dependents
            .get(id)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// Transitive dependents of `id`, not including `id` itself.
    pub fn descendants(&self, id: &str) -> BTreeSet<String> {
        // Peel outward one generation at a time: cheap, terminates on any
        // graph, and avoids the exponential re-walking a naive recursion does.
        std::iter::successors(Some(self.immediate_dependents(id)), |frontier| {
            let next = frontier
                .iter()
                .flat_map(|n| self.dependents(n))
                .cloned()
                .collect::<BTreeSet<_>>();
            (!next.is_empty()).then_some(next)
        })
        .take(self.ids.len() + 1)
        .flatten()
        .collect()
    }

    fn immediate_dependents(&self, id: &str) -> BTreeSet<String> {
        self.dependents(id).iter().cloned().collect()
    }

    /// Repeatedly drop every node whose dependencies are all resolved
    /// (Kahn's algorithm, run as a fixpoint). Whatever survives is exactly the
    /// set of nodes on or downstream of a cycle.
    fn find_cycle(&self) -> Option<Vec<String>> {
        let seed: BTreeSet<String> = self.ids.iter().cloned().collect();
        let remaining = std::iter::successors(Some(seed), |rem| {
            let next: BTreeSet<String> = rem
                .iter()
                .filter(|id| self.needs(id).iter().any(|d| rem.contains(d)))
                .cloned()
                .collect();
            (next.len() < rem.len()).then_some(next)
        })
        .last()?;

        (!remaining.is_empty()).then(|| self.trace_cycle(&remaining))
    }

    /// Walk dependency edges inside `pool` until a node repeats, then return
    /// the closed loop. Every node in `pool` has a dependency in `pool`, so a
    /// walk of `pool.len() + 1` steps must revisit something.
    fn trace_cycle(&self, pool: &BTreeSet<String>) -> Vec<String> {
        let walk: Vec<String> = std::iter::successors(pool.first().cloned(), |cur| {
            self.needs(cur)
                .iter()
                .find(|d| pool.contains(d.as_str()))
                .cloned()
        })
        .take(pool.len() + 1)
        .collect();

        match walk
            .iter()
            .enumerate()
            .find(|(i, node)| walk[..*i].contains(node))
        {
            Some((end, node)) => {
                let start = walk.iter().position(|p| p == node).unwrap_or(0);
                walk[start..=end].to_vec()
            }
            None => walk,
        }
    }
}

#[derive(Debug)]
pub struct Validation {
    pub dag: Option<Dag>,
    pub errors: Vec<ValidationError>,
    pub warnings: Vec<Warning>,
}

fn unparseable_max_duration(t: &Task) -> Option<ValidationError> {
    match &t.max_duration {
        Some(d) if crate::config::parse_duration(d).is_err() => {
            Some(ValidationError::InvalidDuration {
                task: t.id.clone(),
                value: d.clone(),
            })
        }
        _ => None,
    }
}

/// Checks that only apply to agent tasks: the provider must exist, and an
/// agent with no oversight at all is worth flagging.
fn check_agent_task(graph: &Graph, t: &Task) -> (Vec<ValidationError>, Vec<Warning>) {
    match (t.kind, &t.provider) {
        (TaskKind::Agent, Some(name)) => match graph.providers.get(name) {
            None => (
                vec![ValidationError::UnknownProvider {
                    task: t.id.clone(),
                    provider: name.clone(),
                }],
                Vec::new(),
            ),
            Some(p) => {
                let unverified = (t.supervise == Supervise::None && t.verify.is_none())
                    .then(|| Warning::UnsupervisedAgentWithoutVerify(t.id.clone()));
                let uncapped = (t.max_cost_usd.is_some() && p.adapter.is_none()).then(|| {
                    Warning::CostCapWithoutAdapter {
                        task: t.id.clone(),
                        provider: name.clone(),
                    }
                });
                (Vec::new(), unverified.into_iter().chain(uncapped).collect())
            }
        },
        _ => (Vec::new(), Vec::new()),
    }
}

pub fn validate(graph: &Graph) -> Validation {
    let (dag, structural) = match Dag::build(&graph.tasks) {
        Ok(d) => (Some(d), Vec::new()),
        Err(e) => (None, e),
    };

    let (agent_errors, warnings): (Vec<_>, Vec<_>) = graph
        .tasks
        .iter()
        .map(|t| check_agent_task(graph, t))
        .collect::<Vec<_>>()
        .into_iter()
        .unzip();

    let errors: Vec<ValidationError> = structural
        .into_iter()
        .chain(graph.tasks.iter().filter_map(unparseable_max_duration))
        .chain(agent_errors.into_iter().flatten())
        .collect();

    Validation {
        dag: errors.is_empty().then_some(dag).flatten(),
        errors,
        warnings: warnings.into_iter().flatten().collect(),
    }
}
