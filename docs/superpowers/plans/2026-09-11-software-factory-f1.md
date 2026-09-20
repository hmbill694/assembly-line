# Software Factory F1 Implementation Plan — subtraction

**Status:** shipped 2026-09-18

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reduce assembly-line from a DAG engine to a single-job executor —
`assembly run --prompt "..."` takes a repo, a ref and a prompt, and leaves one
branch — with `verify` enforced and per-repo configuration in
`.assembly/config.toml`.

**Architecture:** Pure subtraction, bottom-up, as a jj stack. Each task removes
one concept and leaves the tree building, testing, linting and formatting
cleanly on its own. The DAG, the run branch, shell tasks, the review inbox and
the graph file all go; the worktree machinery, the event log, the fold, and the
provider invocation all stay and get simpler. Nothing new is designed here —
F1 only removes what the factory does not need and reshapes what is left
around one job.

**Tech Stack:** Rust 2024 (stable 1.97.1, pinned), tokio, clap, serde/toml,
anyhow, git via subprocess. jj (colocated with git) for version control.

**Spec:** `docs/superpowers/specs/2026-09-11-software-factory-v2.md`

## Global Constraints

Copied from `CLAUDE.md` and the spec. Every task's requirements implicitly
include this section.

- **Edition 2024, pinned to stable 1.97.1.** Never downgrade the edition to
  work around a compile error — fix the code.
- **All file modifications go through the Edit / Write tools.** Never `sed -i`,
  `perl -pi`, heredocs, `cat >`, `tee`, or inline scripts that write files.
  Reading and searching via shell (`cat`, `sed -n`, `rg`, `find`) is fine.
- **Every change in the jj stack must build, test, lint and format cleanly on
  its own.** `just check` (= `fmt-check` + `lint` + `test`) is the gate.
- **One concern per change**, each carrying the tests for what it adds or
  changes.
- **Functional by default.** Iterators over `for` loops; `match` on the shape
  of data over if-else chains; build values rather than mutating them. The
  exception already granted in `CLAUDE.md` is the scheduler's main loop — and
  after Task 5 there is no loop left to except.
- **Naming.** Predicates read as claims (`dependency_is_satisfied`). Filters
  name what they select. Error producers name the fault. Mutators name the
  transition including its scope. Fields carry their unit or role.
  Constructors say where the value came from (`JobReport::from_events`).
- **No trait until a second implementor exists.** F1 introduces no traits.
- **Validation returns a `Vec` of typed errors**, not the first failure.
- **The event log is append-only.** Never rewritten or truncated.
- **`JobState` must stay a pure function of the event stream.**
- **Ids match `^[A-Za-z0-9_-]+$`.** Validate, never sanitize.
- **The target repository is never modified.** Worktrees live outside it.
- **No test may touch the network or require credentials.** Agent execution is
  tested with the shell-script fakes in `tests/fixtures/`.
- **Test counts should climb monotonically up the stack** — except here. This
  is a subtraction milestone; counts fall as concepts are removed. Record the
  count after each task in the commit body so the drop is deliberate and
  visible rather than silent.

## Version control

This repo uses jj colocated with git. After each task:

```bash
jj describe -m "<conventional commit subject>

<body: what this removes, and the test count after>"
jj new
```

Do **not** `jj edit` down the stack to verify. Use `just verify-stack`, which
exports each revision with `git archive` and builds it in a temp directory.

## File Structure

What each file is responsible for once F1 is done.

| File | Responsibility after F1 |
|---|---|
| `src/cli.rs` | `run`, `revise`, `status`, `logs`, `gc` |
| `src/config.rs` | `RepoConfig` — one repo's factory settings, read from a ref |
| `src/event.rs` | The job event vocabulary and the append-only log |
| `src/state.rs` | `JobState` — a fold over one job's events |
| `src/report.rs` | `JobReport` — the human-readable fold |
| `src/paths.rs` | Where a job's state and worktree live |
| `src/workspace.rs` | One job's scratch checkout: create, commit, publish, discard |
| `src/git.rs` | Git subprocess operations |
| `src/exec.rs` | Running a command with a timeout, into a log file |
| `src/provider.rs` | Provider config → `CommandSpec` |
| `src/job.rs` | `run_job` — one job end to end |
| `src/delivery.rs` | Opening a pull request for a finished branch |
| `src/gc.rs` | Collecting orphaned worktrees |
| `src/main.rs` | CLI wiring and printing |
| **deleted** | `src/dag.rs`, `src/review.rs` |

Test files after F1: `config.rs`, `event_log.rs`, `exec.rs`, `gc.rs`, `git.rs`,
`job.rs`, `paths.rs`, `report.rs`, `state.rs`, `verify.rs`, `workspace.rs`,
`cli.rs`, `delivery.rs`, `provider.rs`.
Deleted: `review.rs`, `validate.rs`, `scheduler.rs`, `resume.rs`,
`agent_nodes.rs` (becomes `job.rs`), `failure.rs`, `prompt_file.rs`.

---

### Task 1: The pull request is the inbox

Removes supervision and the review inbox. Review moves to the pull request in
F4; nothing in the factory gates a merge on a human at a TTY.

**Files:**
- Delete: `src/review.rs`, `tests/review.rs`
- Modify: `src/lib.rs`, `src/cli.rs`, `src/main.rs`, `src/event.rs`,
  `src/state.rs`, `src/report.rs`, `src/config.rs`, `src/dag.rs`,
  `src/scheduler.rs`
- Test: `tests/cli.rs`, `tests/event_log.rs`, `tests/state.rs`,
  `tests/validate.rs`, `tests/agent_nodes.rs`

**Interfaces:**
- Consumes: nothing (first task)
- Produces: `EventKind` with no `NodeAwaitingReview`, `NodeApproved` or
  `NodeRevisionRequested` variants. `Command::Revise { run_id: u64, node:
  String, feedback: String }` — feedback is now required, not optional.
  `dag::Warning::AgentWithoutVerify(String)` replaces
  `UnsupervisedAgentWithoutVerify`.

- [ ] **Step 1: Delete the review module and its tests**

```bash
rm src/review.rs tests/review.rs
```

Then remove `pub mod review;` from `src/lib.rs`.

- [ ] **Step 2: Remove the review command from the CLI**

In `src/cli.rs`, delete the whole `Review { .. }` variant. Change `Revise` so
feedback is required — with no inbox there is nowhere for it to have been
recorded:

```rust
    /// Run a node again, based on its own branch, with feedback
    ///
    /// A new job, not a resumption: the agent's prior work arrives as files on
    /// disk, and this round appends to the node's branch.
    Revise {
        run_id: u64,
        node: String,
        /// What to change about the previous round's work
        feedback: String,
    },
```

- [ ] **Step 3: Remove the review events**

In `src/event.rs`, delete the `NodeAwaitingReview`, `NodeApproved` and
`NodeRevisionRequested` variants and their arms in `EventKind::node()`.

In `src/state.rs` and `src/report.rs`, delete those three names from the
exhaustive match arms. Both matches list every variant deliberately so a new
event is a compile error — keep that property, just with three fewer names.

- [ ] **Step 4: Remove supervision from config and the scheduler**

In `src/config.rs`, delete the `Supervise` enum and the `supervise` field on
`Task`.

In `src/scheduler.rs`:
- delete the `Supervise` import
- delete the `gated` parameter from `record_completion` and
  `events_for_completion`, and the `deferred_gate` vector inside the latter
- at the two call sites (`revise_node`, and the settle branch of `execute`),
  drop the `task.supervise != Supervise::None` argument and the `gated` local

`events_for_completion` becomes:

```rust
/// The events a completion implies, in the order things settled, paired with
/// whether the node ended up failed.
///
/// Pure, so the ordering that makes replay correct can be asserted without
/// running anything. Work is always recorded first: a failure that produced a
/// diff still produced a diff.
fn events_for_completion(node: &str, result: NodeResult) -> (Vec<EventKind>, bool) {
```

- [ ] **Step 5: Rename the verify warning**

In `src/dag.rs`, `UnsupervisedAgentWithoutVerify` names a supervision mode
that no longer exists. Rename the variant and reword the message:

```rust
    AgentWithoutVerify(String),
```

```rust
            Self::AgentWithoutVerify(id) => write!(
                f,
                "agent task '{id}' declares no `verify` — nothing will check its output"
            ),
```

Update its construction site in `agent_only_checks` and the
`Supervise` import at the top of `src/dag.rs`.

- [ ] **Step 6: Remove review wiring from main**

In `src/main.rs`, delete `review_run`, `nodes_built_on`, `report_blast_radius`,
`feedback_recorded_for`, the `ReviewInbox`/`ReviewState` import, and the
`Command::Review` match arm. In `revise_node_of_run`, change the signature to
take `feedback: String` and delete the `feedback.or_else(...)` fallback and its
error branch — clap now guarantees the value.

- [ ] **Step 7: Update the tests**

In `tests/cli.rs`, delete every test exercising `assembly review` and any that
asserts on approval events. In `tests/event_log.rs` and `tests/state.rs`,
delete cases feeding the three removed events. In `tests/validate.rs`, update
the warning test to the new name. In `tests/agent_nodes.rs`, delete
assertions on `NodeAwaitingReview`.

- [ ] **Step 8: Run the gate**

Run: `just check`
Expected: PASS. Note the test count.

- [ ] **Step 9: Commit**

```bash
jj describe -m "refactor(review): the pull request is the inbox

Supervision gated a merge on a human at a TTY. In the factory, review
happens on the pull request, so the inbox, the supervise field and the
three review events have nowhere to attach.

Tests: <N> (was <M>)."
jj new
```

---

### Task 2: Every task is an agent

Removes shell tasks. `verify` is a field on a job, not a kind of node.
`exec::run_shell` stays — Task 6 needs it to run `verify`.

**Files:**
- Modify: `src/config.rs`, `src/dag.rs`, `src/scheduler.rs`, `src/main.rs`
- Test: `tests/validate.rs`, `tests/scheduler.rs`, `tests/failure.rs`,
  `tests/cli.rs`, `tests/agent_nodes.rs`, `justfile`

**Interfaces:**
- Consumes: Task 1's `events_for_completion(node, result)`.
- Produces: `config::Task` with no `kind` or `run` fields. `TaskKind` gone.
  `dag::ValidationError` with no `ShellMissingRun`.

- [ ] **Step 1: Write the failing test**

In `tests/validate.rs`, replace the shell-missing-run case with one asserting a
task needs a prompt regardless of how it is written:

```rust
#[test]
fn a_task_without_a_prompt_is_rejected() {
    let graph = parse_graph(
        r#"
        [[task]]
        id = "impl"
        "#,
    )
    .unwrap();

    let validation = validate(&graph);
    assert!(
        validation
            .errors
            .iter()
            .any(|e| matches!(e, ValidationError::AgentMissingPrompt(id) if id == "impl")),
        "expected a missing-prompt error, got {:?}",
        validation.errors
    );
}
```

- [ ] **Step 2: Run it to make sure it fails**

Run: `cargo test --test validate a_task_without_a_prompt_is_rejected`
Expected: FAIL — the TOML has no `kind`, which is still a required field.

- [ ] **Step 3: Remove the kind**

In `src/config.rs`, delete `TaskKind` and the `kind` and `run` fields from
`Task`.

In `src/dag.rs`: delete `ShellMissingRun` and its `Display` arm; delete the
`TaskKind` import; reduce `missing_required_field` to:

```rust
/// A task must carry a prompt to be runnable at all. It may supply it inline
/// or by file; `load_graph` folds the latter into the former, so either
/// satisfies this check.
fn missing_required_field(t: &Task) -> Option<ValidationError> {
    match (&t.prompt, &t.prompt_file) {
        (None, None) => Some(ValidationError::AgentMissingPrompt(t.id.clone())),
        _ => None,
    }
}
```

Delete the `agent_only_checks` guard that filtered on `TaskKind::Agent` — every
task is one now, so its checks apply to all.

- [ ] **Step 4: Remove the shell branch from the scheduler**

In `src/scheduler.rs`:
- delete the `TaskKind` import and the `run_shell` import
- delete the `graph_has_agent_nodes` local and the `(false, _)` arm of the
  `run_branch` match — a graph always has agent nodes now, so the run branch
  depends only on whether a repo was given
- delete the `shell_cwd` local
- replace the `match task.kind { ... }` block in `execute` with the agent arm's
  body alone
- in `revise_node`, delete the `anyhow::ensure!(task.kind == TaskKind::Agent, ...)`
  check

- [ ] **Step 5: Update main and the demo**

In `src/main.rs`, nothing references `TaskKind` directly — verify with
`rg -n 'TaskKind|kind = ' src/`. In `justfile`, the `demo` recipe writes a
shell-only graph; delete the recipe. F1's Task 5 gives it a replacement worth
having, and a demo that cannot run is worse than none.

- [ ] **Step 6: Update the tests**

`tests/scheduler.rs` and `tests/failure.rs` are built almost entirely on shell
tasks. Rewrite each surviving case to use the `tests/fixtures/fake-agent.sh`
provider block that `tests/agent_nodes.rs` already defines, or delete the case
where it was testing shell-specific behavior. Remove `kind = "shell"` and
`kind = "agent"` from every TOML fixture string in `tests/`.

- [ ] **Step 7: Run the gate**

Run: `just check`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
jj describe -m "refactor(scheduler): every task is an agent

A user-authored shell node has no place in a factory whose input is an
issue. Checking the work is what `verify` is for, and that is a field.

Tests: <N> (was <M>)."
jj new
```

---

### Task 3: A job's branch is cut from its base ref

Removes the run branch, the integration worktree, and every merge. A node
branches from the base and stops there. This is the change that makes a job
stateless with respect to other jobs.

**Files:**
- Modify: `src/scheduler.rs`, `src/event.rs`, `src/state.rs`, `src/report.rs`,
  `src/paths.rs`, `src/workspace.rs`, `src/git.rs`, `src/main.rs`
- Test: `tests/agent_nodes.rs`, `tests/git.rs`, `tests/paths.rs`,
  `tests/workspace.rs`, `tests/cli.rs`

**Interfaces:**
- Consumes: Task 2's `Task` with no `kind`.
- Produces: `EventKind` with no `RunBranchCreated`, `NodeMerged` or
  `NodeMergeConflicted`. `git` with no `MergeOutcome`, `merge_branch`,
  `abort_merge` or `conflicted_paths`. `paths::RunPaths` with no
  `integration_worktree`. `workspace` with no `run_branch_name`.

- [ ] **Step 1: Write the failing test**

In `tests/agent_nodes.rs`, assert that a node's branch is cut from the repo's
own HEAD and that no integration worktree is created:

```rust
#[tokio::test]
async fn a_node_branches_from_the_repository_head() {
    let h = Harness::new().await;
    let base = head_sha(&h.repo).await.unwrap();

    let outcome = h
        .run(
            &format!(
                "{}\n[[task]]\nid = \"impl\"\nprovider = \"fake\"\nprompt = \"go\"\n",
                provider_block("fake-agent.sh", "one")
            ),
            1,
        )
        .await;

    assert_eq!(outcome.status, RunStatus::Ok);
    let branch = format!("al/run-{}-impl", outcome.run_id);
    let parent = git::run_allowing_failure(&h.repo, &["rev-parse", &format!("{branch}^")])
        .await
        .unwrap();
    assert_eq!(parent.stdout.trim(), base, "branch should sit directly on HEAD");

    assert!(
        !outcome.has(|e| matches!(e, EventKind::NodeMerged { .. })),
        "nothing merges any more"
    );
}
```

- [ ] **Step 2: Run it to make sure it fails**

Run: `cargo test --test agent_nodes a_node_branches_from_the_repository_head`
Expected: FAIL — the node currently branches from the run branch, and a
`NodeMerged` event is emitted.

- [ ] **Step 3: Remove the run branch from the scheduler**

In `src/scheduler.rs`, delete: the `RunBranch` struct and its `impl`, the
`MergeRecord` enum, the `merge` field of `NodeResult::Committed`, the
`run_branch` field of `AgentNodePlan`, `prepare_run_branch`, and
`check_out_run_branch`.

`AgentNodePlan` gains the two things `RunBranch` was carrying for it:

```rust
struct AgentNodePlan {
    node: String,
    repo: PathBuf,
    base_sha: String,
    workspace_path: PathBuf,
    branch: String,
    seed_from: PathBuf,
    copy_paths: Vec<String>,
    command: CommandSpec,
    commit_message: String,
    remote: String,
    /// A first attempt cuts a fresh branch off the base; a revise round
    /// continues the node's own branch, so the agent starts from its prior
    /// work.
    continues_branch: bool,
}
```

`NodeResult::Committed` loses its merge:

```rust
    /// An agent left work, which was committed and published. The branch is
    /// the whole durable output of a job.
    Committed { work: AgentWork },
```

`agent_node_result` ends after publishing:

```rust
    if let Some(reason) = ran.failure_reason() {
        return Ok(NodeResult::Failed { reason, work });
    }

    Ok(match work {
        None => NodeResult::Succeeded,
        Some(work) => NodeResult::Committed { work },
    })
```

`events_for_completion`'s `Committed` arm collapses to the landed case with no
merge event:

```rust
        NodeResult::Committed { work } => (
            work_recorded(node, Some(&work))
                .into_iter()
                .chain([finished])
                .collect(),
            false,
        ),
```

In `execute`, delete the `run_branch` local and the `RunBranchCreated` append;
`agent_node_plan` now takes the repo and base sha directly.

- [ ] **Step 4: Remove the merge events and git operations**

In `src/event.rs`, delete `RunBranchCreated`, `NodeMerged` and
`NodeMergeConflicted`, and their arms in `node()`. Remove the same three names
from the exhaustive matches in `src/state.rs` and `src/report.rs`.

In `src/git.rs`, delete `MergeOutcome`, `merge_branch`, `conflicted_paths` and
`abort_merge`. In `tests/git.rs`, delete the merge and conflict cases.

- [ ] **Step 5: Remove the integration worktree**

In `src/paths.rs`, delete `INTEGRATION_WORKTREE` and
`RunPaths::integration_worktree`. In `src/workspace.rs`, delete
`run_branch_name`. In `src/dag.rs`, the `ReservedId` check exists because ids
became directory names beside `_integration`; keep the check — `_` stays
reserved for assembly-line's own use — but reword the message:

```rust
            Self::ReservedId(id) => write!(
                f,
                "task id '{id}' is reserved: ids may not start with '_', which assembly-line keeps for itself"
            ),
```

- [ ] **Step 6: Remove run-branch handling from main**

In `src/main.rs`, delete `record_run_branch_in_meta` and `branch_creation`.
`deliver_if_complete` reads `meta.run_branch`, which is now never set — delete
the function and its call site too. Delivery returns in F4 against the job's
own branch; carrying a dead path through the stack is worse than a gap.

- [ ] **Step 7: Run the gate**

Run: `just check`
Expected: PASS, including the new test from Step 1.

- [ ] **Step 8: Commit**

```bash
jj describe -m "refactor(scheduler): a job's branch is cut from its base ref

The run branch existed so a dependent node could see its upstream's work.
With one job per issue there are no dependents, so the integration
worktree, the merge lock and every merge outcome go with it.

Tests: <N> (was <M>)."
jj new
```

---

### Task 4: Tasks no longer depend on each other

Deletes the DAG. What remains is a list of independent tasks, all ready at
once — a transient shape that Task 5 collapses to exactly one.

**Files:**
- Delete: `src/dag.rs`, `tests/validate.rs`, `tests/scheduler.rs`,
  `tests/failure.rs`
- Create: `tests/config.rs` gains the validation cases worth keeping
- Modify: `src/lib.rs`, `src/config.rs`, `src/state.rs`, `src/scheduler.rs`,
  `src/report.rs`, `src/event.rs`, `src/main.rs`

**Interfaces:**
- Consumes: Task 3's `AgentNodePlan { repo, base_sha, .. }`.
- Produces: `config::validate(&Graph) -> Validation` moves from `dag` to
  `config`, keeping `Validation { errors: Vec<ValidationError>, warnings:
  Vec<Warning> }` but with no `dag` field. `EventKind` with no `NodeSkipped`.
  `state::RunState` with no `ready`, `dependency_is_satisfied` or `Dag`
  dependency.

- [ ] **Step 1: Move the surviving validation into config**

Create `tests/config.rs` cases for the checks that still mean something:
duplicate ids, invalid ids, reserved ids, missing prompt, unknown provider,
invalid duration. Example:

```rust
use assembly_line::config::{ValidationError, parse_graph, validate};

#[test]
fn an_id_that_cannot_be_a_branch_name_is_rejected() {
    let graph = parse_graph(
        r#"
        [[task]]
        id = "impl auth"
        prompt = "go"
        "#,
    )
    .unwrap();

    assert!(
        validate(&graph)
            .errors
            .iter()
            .any(|e| matches!(e, ValidationError::InvalidId(id) if id == "impl auth")),
    );
}
```

- [ ] **Step 2: Run it to make sure it fails**

Run: `cargo test --test config an_id_that_cannot_be_a_branch_name_is_rejected`
Expected: FAIL — `validate` and `ValidationError` are not in `config` yet.

- [ ] **Step 3: Move validation, drop the graph**

Move `ValidationError`, `Warning`, `Validation`, `id_is_valid`,
`id_naming_errors`, `missing_required_field`, `agent_checks` and `validate`
from `src/dag.rs` into `src/config.rs`. Drop from them: `SelfDep`,
`UnknownDep`, `Cycle`, `unresolvable_dependencies`, and the `dag` field of
`Validation`. `validate` becomes:

```rust
/// Every problem in a graph file, so a user fixes all of them in one pass
/// rather than one per run.
#[must_use]
pub fn validate(graph: &Graph) -> Validation {
    Validation {
        errors: id_naming_errors(&graph.tasks)
            .chain(graph.tasks.iter().filter_map(missing_required_field))
            .chain(graph.tasks.iter().filter_map(|t| unknown_provider(graph, t)))
            .chain(graph.tasks.iter().filter_map(unparseable_max_duration))
            .collect(),
        warnings: graph.tasks.iter().filter_map(missing_verify).collect(),
    }
}
```

Delete `src/dag.rs` and `pub mod dag;` from `src/lib.rs`.

- [ ] **Step 4: Remove dependencies from the task and the state**

In `src/config.rs`, delete the `needs`, `resource` and `on_failure` fields from
`Task`, and the `OnFailure` enum.

In `src/state.rs`, delete `ready`, `dependency_is_satisfied`, and the `Dag`
import. `RunState` keeps `new`, `state`, `apply`, `replay`, `reset_running`
and `counts`.

In `src/event.rs`, delete `NodeSkipped` and its `node()` arm; remove the name
from the exhaustive matches in `src/state.rs` and `src/report.rs`, and delete
`NodeState::Skipped` along with its `Counts.skipped` field, its glyph in
`report::state_glyph`, and `RunReport::to_summary_line`'s skipped column.

- [ ] **Step 5: Simplify the scheduler loop**

In `src/scheduler.rs`, delete `nodes_clear_to_launch`,
`mark_pending_as_skipped`, the `resources_in_use` set, the `aborting` flag and
the `OnFailure` match. `execute` launches every pending task, bounded by
`opts.jobs`:

```rust
/// Tasks not yet started, up to the job cap.
fn tasks_clear_to_launch(state: &RunState, graph: &Graph, free_slots: usize) -> Vec<String> {
    graph
        .tasks
        .iter()
        .filter(|t| state.state(&t.id) == NodeState::Pending)
        .map(|t| t.id.clone())
        .take(free_slots)
        .collect()
}
```

`execute` drops its `dag: &Dag` parameter. The settle branch becomes:

```rust
        let completion = joined?;
        record_completion(log, state, &completion.node, completion.result)?;
```

- [ ] **Step 6: Update main**

In `src/main.rs`, replace `dag::validate` with `config::validate` throughout,
delete the `validation.dag` destructuring in `drive_run` (validation now only
reports errors), and drop the `dag` argument at the `execute` call site.

- [ ] **Step 7: Run the gate**

Run: `just check`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
jj describe -m "refactor(dag): tasks no longer depend on each other

Ordering between pieces of work is expressed by filing two issues, and a
conflict between them is the tester loop's input. Cycle detection,
resource exclusivity and skip-subtree have nothing left to order.

Tests: <N> (was <M>)."
jj new
```

---

### Task 5: A job is a repo, a ref and a prompt

The pivot. The graph file is replaced by a prompt on the command line plus
`.assembly/config.toml` read from the base ref. One job per run.

**Files:**
- Create: `tests/repo_config.rs`
- Delete: `tests/prompt_file.rs`, `tests/resume.rs`
- Rename: `tests/agent_nodes.rs` → `tests/job.rs`
- Modify: `src/git.rs`, `src/config.rs`, `src/cli.rs`, `src/main.rs`,
  `src/scheduler.rs`, `src/paths.rs`, `src/workspace.rs`, `src/state.rs`,
  `src/report.rs`, `tests/cli.rs`

**Interfaces:**
- Consumes: Task 4's `config::validate`.
- Produces:
  - `git::file_at_ref(repo, git_ref, path) -> anyhow::Result<Option<String>>`
  - `config::RepoConfig { provider: Option<String>, verify: Option<String>,
    base: Option<String>, max_duration: Option<String>, copy: Vec<String>,
    providers: BTreeMap<String, Provider> }`
  - `config::RepoConfig::from_ref(repo: &Path, git_ref: &str) -> anyhow::Result<RepoConfig>`
  - `scheduler::JobSpec<'a> { prompt: &'a str, provider: &'a str, base_ref: &'a str, round: u32 }`
  - `scheduler::run_job(config, spec, paths, log, state, opts) -> anyhow::Result<bool>`
  - `paths::JobMeta { repo, base_ref, prompt, provider, branch }`
  - `workspace::job_branch_name(job_id: u64) -> String` → `al/job-<id>`

- [ ] **Step 1: Write the failing test for reading a file at a ref**

In `tests/git.rs`:

```rust
#[tokio::test]
async fn a_file_is_read_from_a_ref_not_the_working_tree() {
    let repo = repo_with_initial_commit().await;
    std::fs::write(repo.path().join("marker"), "committed\n").unwrap();
    git::commit_all(repo.path(), "add marker").await.unwrap();

    // The working tree disagrees with HEAD.
    std::fs::write(repo.path().join("marker"), "edited\n").unwrap();

    let at_head = git::file_at_ref(repo.path(), "HEAD", "marker")
        .await
        .unwrap();
    assert_eq!(at_head.as_deref(), Some("committed\n"));

    let absent = git::file_at_ref(repo.path(), "HEAD", "nope").await.unwrap();
    assert_eq!(absent, None, "a missing path is None, not an error");
}
```

Use whatever repo-fixture helper `tests/git.rs` already defines rather than
`repo_with_initial_commit` if the name differs — check the top of the file.

- [ ] **Step 2: Run it to make sure it fails**

Run: `cargo test --test git a_file_is_read_from_a_ref_not_the_working_tree`
Expected: FAIL with "no function `file_at_ref`".

- [ ] **Step 3: Implement file_at_ref**

In `src/git.rs`:

```rust
/// One file's contents as of `git_ref`, or `None` when that ref does not carry
/// it.
///
/// Reading configuration from a ref rather than from a checkout is what stops
/// a job editing the settings that govern it: the agent's branch can say
/// anything, and this never looks at it.
pub async fn file_at_ref(
    repo: impl AsRef<Path>,
    git_ref: &str,
    path: &str,
) -> anyhow::Result<Option<String>> {
    let spec = format!("{git_ref}:{path}");
    let present = run_allowing_failure(&repo, &["cat-file", "-e", &spec])
        .await?
        .succeeded();

    match present {
        false => Ok(None),
        true => run_expecting_success(&repo, &["show", &spec], &format!("show {spec}"))
            .await
            .map(Some),
    }
}
```

Note `run_expecting_success` trims — for a config file that is harmless, and
it keeps the one code path every other reader here uses.

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test --test git a_file_is_read_from_a_ref_not_the_working_tree`
Expected: PASS.

- [ ] **Step 5: Write the failing test for RepoConfig**

Create `tests/repo_config.rs`:

```rust
use assembly_line::config::RepoConfig;

mod support;

#[tokio::test]
async fn a_repo_declares_how_the_factory_builds_it() {
    let repo = support::repo_with_initial_commit().await;
    std::fs::create_dir_all(repo.path().join(".assembly")).unwrap();
    std::fs::write(
        repo.path().join(".assembly/config.toml"),
        r#"
provider = "claude"
verify = "cargo test"
base = "main"
copy = [".env"]

[providers.claude]
cmd = "claude"
args = ["-p", "{prompt}"]
"#,
    )
    .unwrap();
    assembly_line::git::commit_all(repo.path(), "add config")
        .await
        .unwrap();

    let config = RepoConfig::from_ref(repo.path(), "HEAD").await.unwrap();

    assert_eq!(config.provider.as_deref(), Some("claude"));
    assert_eq!(config.verify.as_deref(), Some("cargo test"));
    assert_eq!(config.copy, vec![".env".to_string()]);
    assert!(config.providers.contains_key("claude"));
}

#[tokio::test]
async fn a_repo_with_no_config_is_not_opted_in() {
    let repo = support::repo_with_initial_commit().await;

    let err = RepoConfig::from_ref(repo.path(), "HEAD").await.unwrap_err();

    assert!(
        err.to_string().contains(".assembly/config.toml"),
        "the error should name the file to create, got: {err}"
    );
}
```

Create `tests/support/mod.rs` holding `repo_with_initial_commit` — lift the
harness setup from `tests/agent_nodes.rs`'s `Harness::new` so both files share
one definition rather than a third copy.

- [ ] **Step 6: Run it to make sure it fails**

Run: `cargo test --test repo_config`
Expected: FAIL with "no type `RepoConfig`".

- [ ] **Step 7: Implement RepoConfig**

In `src/config.rs`, replace `Graph`, `Workspace` and `Task` with:

```rust
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

/// Where a repository declares its factory settings.
pub const REPO_CONFIG_PATH: &str = ".assembly/config.toml";

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

        toml::from_str(&src).map_err(|e| anyhow::anyhow!("parsing {REPO_CONFIG_PATH}: {e}"))
    }
}
```

Delete `parse_graph`, `load_graph`, `inline_prompt_files`, `Hook`, the
`hooks` field, `output_file`, `max_cost_usd`, `retries`, and
`DeliveryMode::Push`. Keep `Provider`, `Delivery`, `parse_duration`, and the
validation moved in by Task 4 — reshaped in Step 9 below.

- [ ] **Step 8: Run it to verify it passes**

Run: `cargo test --test repo_config`
Expected: PASS.

- [ ] **Step 9: Reshape validation onto RepoConfig**

Validation is now about the repo's config, not a task list:

```rust
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
```

`RepoConfig::problems(&self, provider: &str) -> Vec<ConfigError>` and
`RepoConfig::warnings(&self) -> Vec<Warning>` replace `validate`. Move the
`tests/config.rs` cases from Task 4 onto these; delete the id-naming cases —
there are no task ids any more. Job ids are integers assembly-line allocates.

- [ ] **Step 10: Write the failing test for one job per run**

Rename `tests/agent_nodes.rs` to `tests/job.rs` and reduce its harness to one
job. The central case:

```rust
#[tokio::test]
async fn a_job_leaves_one_branch_carrying_its_work() {
    let h = Harness::new().await;

    let outcome = h.run_job("write a file").await;

    assert!(outcome.succeeded);
    assert!(
        outcome.has(|e| matches!(e, EventKind::JobCommitted { .. })),
        "the agent's work should be committed"
    );
    assert!(
        git::branch_exists(&h.repo, &job_branch_name(outcome.job_id))
            .await
            .unwrap(),
        "the branch is the job's whole durable output"
    );
    assert!(
        !h.worktree_root().exists(),
        "the checkout is scratch and never survives"
    );
}
```

- [ ] **Step 11: Run it to make sure it fails**

Run: `cargo test --test job a_job_leaves_one_branch_carrying_its_work`
Expected: FAIL — `run_job`, `JobCommitted` and `job_branch_name` do not exist.

- [ ] **Step 12: Collapse the scheduler to one job**

In `src/scheduler.rs`, delete `execute`, `NodeCompletion`,
`tasks_clear_to_launch`, the `JoinSet`, and `RunOpts::jobs`. What remains is
one function and the plan it builds:

```rust
/// One job: which prompt, run by which provider, against which ref.
#[derive(Debug, Clone, Copy)]
pub struct JobSpec<'a> {
    pub prompt: &'a str,
    pub provider: &'a str,
    pub base_ref: &'a str,
    /// 1 for a first attempt; higher for a revise round, which continues the
    /// job's own branch.
    pub round: u32,
}

/// Run one job end to end: scratch checkout, agent, commit, publish, discard.
///
/// Returns whether the job failed.
///
/// # Errors
///
/// Returns an error only if the job cannot be *administered* — the event log
/// cannot be appended to, `max_duration` is unparseable, or git refused to
/// make a checkout. An agent that runs and fails is not an error: that is the
/// returned flag.
pub async fn run_job(
    config: &RepoConfig,
    spec: &JobSpec<'_>,
    paths: &JobPaths,
    log: &mut EventLog,
    state: &mut JobState,
    opts: &RunOpts,
) -> anyhow::Result<bool> {
```

Its body is `agent_node_result`'s, with `plan.node` gone and the round taken
from `spec`. `agent_node_plan` becomes `job_plan(config, spec, paths, opts)`
and resolves the provider from `config.providers`, erroring with
`ConfigError::UnknownProvider` rather than a bare string.

`revise_node` becomes a thin caller — a revise round is a job with
`round > 1` and a prompt built by `revised_prompt`:

```rust
/// Another round on this job's branch, with feedback folded into the prompt.
///
/// A revise is a *new job*, not a resumption. Nothing is kept from the last
/// round except the branch — which is exactly what the agent needs, because
/// its prior work arrives as files on disk.
pub async fn revise_job(
    config: &RepoConfig,
    meta: &JobMeta,
    feedback: &str,
    round: u32,
    paths: &JobPaths,
    log: &mut EventLog,
    state: &mut JobState,
    opts: &RunOpts,
) -> anyhow::Result<bool> {
    let prompt = revised_prompt(&meta.prompt, feedback);
    let spec = JobSpec {
        prompt: &prompt,
        provider: &meta.provider,
        base_ref: &meta.base_ref,
        round,
    };
    run_job(config, &spec, paths, log, state, opts).await
}
```

- [ ] **Step 13: Collapse the events, state and report**

In `src/event.rs`, replace every `Node*` and `Run*` variant with the job
vocabulary. A job's identity lives in `meta.json`, so no event repeats it:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum EventKind {
    /// A round began. Round 1 is the first attempt; higher rounds are revises.
    JobStarted { round: u32 },
    /// The checkout had changes, now recorded on the job's branch.
    JobCommitted {
        sha: String,
        files: usize,
        insertions: usize,
        deletions: usize,
    },
    /// The branch was made durable. `pushed_to` names the remote it reached,
    /// or is `None` when the repository has none — a complete outcome, not a
    /// degraded one.
    ///
    /// Emitted for failed jobs too: a job leaves nothing but its branch, so
    /// this is what makes a failure inspectable at all.
    JobBranchPublished {
        branch: String,
        pushed_to: Option<String>,
    },
    JobFinished { exit_code: i32 },
    JobFailed { reason: String },
}
```

Delete `RunStatus` and `EventKind::node()`.

In `src/state.rs`, `RunState` collapses to the job's own execution state:

```rust
/// A job's state, derived purely from its event stream.
///
/// Nothing may live here that cannot be reconstructed from `events.jsonl`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum JobState {
    #[default]
    Pending,
    Running,
    Succeeded,
    Failed,
}

impl JobState {
    pub fn apply(&mut self, kind: &EventKind) {
        *self = match kind {
            EventKind::JobStarted { .. } => JobState::Running,
            EventKind::JobFinished { .. } => JobState::Succeeded,
            EventKind::JobFailed { .. } => JobState::Failed,
            // Progress markers, not transitions. Listed one by one rather than
            // behind a catch-all, so a new event is a compile error here
            // instead of a silent omission.
            EventKind::JobCommitted { .. } | EventKind::JobBranchPublished { .. } => *self,
        };
    }

    #[must_use]
    pub fn replay<'a>(events: impl IntoIterator<Item = &'a Event>) -> Self {
        events
            .into_iter()
            .fold(JobState::default(), |mut st, e| {
                st.apply(&e.kind);
                st
            })
    }
}
```

Delete `TaskMap`, `task_map`, `Counts` and `reset_running`.

In `src/report.rs`, `RunReport` becomes `JobReport`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobReport {
    pub id: u64,
    pub state: JobState,
    /// How many rounds this job has had. 1 unless it has been revised.
    pub rounds: u32,
    /// Wall time of the most recent round.
    pub duration: Option<Duration>,
    /// What the job committed, or `None` if the agent changed nothing.
    pub diff: Option<DiffSummary>,
    /// Failure reason, when there is one.
    pub detail: Option<String>,
    pub branch: Option<String>,
}

impl JobReport {
    pub fn from_events<'a>(id: u64, events: impl IntoIterator<Item = &'a Event>) -> Self { .. }

    /// The single line printed at the end of a job.
    #[must_use]
    pub fn to_summary_line(&self) -> String { .. }
}
```

`to_terminal_tree` goes — a job has no tree. `to_summary_line` reads
`job 7: failed (round 2, 3 files +40/-2) — verify failed`.

- [ ] **Step 14: Rework paths for jobs**

In `src/paths.rs`: `runs_root` → `jobs_root` (`<git-root>/.assembly/jobs`),
`RunPaths` → `JobPaths`, `create_run`/`open_run`/`next_run_id`/`latest_run_id`
→ the `job` spellings. `JobPaths::log(&self)` takes no node and returns
`<dir>/job.log`. `node_worktree` → `worktree`. `RunMeta` → `JobMeta`:

```rust
/// Enough to reconstruct a job from its id alone — `revise` needs the prompt
/// it is revising, and the ref it was cut from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobMeta {
    pub repo: PathBuf,
    pub base_ref: String,
    pub prompt: String,
    pub provider: String,
    /// Set once the job publishes its branch.
    #[serde(default)]
    pub branch: Option<String>,
}
```

In `src/workspace.rs`, `node_branch_name(run_id, node)` becomes:

```rust
#[must_use]
pub fn job_branch_name(job_id: u64) -> String {
    format!("al/job-{job_id}")
}
```

- [ ] **Step 15: Rework the CLI**

`src/cli.rs`:

```rust
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run one job: an agent, a prompt, and the branch it leaves
    Run {
        /// What the agent is asked to do
        #[arg(long, conflicts_with = "prompt_file")]
        prompt: Option<String>,
        /// Read the prompt from a file instead
        #[arg(long)]
        prompt_file: Option<PathBuf>,
        /// The repository to work in. Defaults to the enclosing one.
        #[arg(long)]
        repo: Option<PathBuf>,
        /// What to branch from. Defaults to the checked-out branch.
        #[arg(long = "ref")]
        base_ref: Option<String>,
        /// Overrides the repository's declared provider
        #[arg(long)]
        provider: Option<String>,
    },

    /// Run a job again, based on its own branch, with feedback
    Revise {
        job_id: u64,
        /// What to change about the previous round's work
        feedback: String,
    },

    /// Show a job's state, timing and diff
    Status {
        /// Defaults to the most recent job
        job_id: Option<u64>,
    },

    /// Print a job's captured output
    Logs {
        job_id: u64,
        /// Follow the log as it grows
        #[arg(short, long)]
        follow: bool,
    },

    /// Remove worktrees left behind by jobs that died mid-run
    Gc {
        /// Also remove worktrees untouched for this long, e.g. "7d"
        #[arg(long)]
        older_than: Option<String>,
        /// Report what would be removed, and remove nothing
        #[arg(long)]
        dry_run: bool,
    },
}
```

Delete `Validate` — validation now happens as part of `run`, against the
repo's own config, and there is no user-authored file to check ahead of time.
Delete `Resume`: an interrupted job is re-dispatched, and its branch already
carries whatever it managed. The daemon reconciles properly in F3.

In `src/main.rs`, rewrite the command handlers against the new shapes, and
delete `drive_run`, `continue_existing_run`, `print_run_outcome`,
`report_for`'s graph argument, and `rounds_so_far`'s event filter (it now
counts `JobStarted`).

- [ ] **Step 16: Delete the orphaned tests**

`rm tests/prompt_file.rs tests/resume.rs`. Update `tests/cli.rs` to the new
command surface throughout.

- [ ] **Step 17: Run the gate**

Run: `just check`
Expected: PASS.

- [ ] **Step 18: Commit**

```bash
jj describe -m "feat(job): a job is a repo, a ref and a prompt

The graph file is gone. A job's prompt comes from the command line and
everything else from .assembly/config.toml, read from the base ref so a
job cannot edit the settings that govern it.

Tests: <N> (was <M>)."
jj new
```

---

### Task 6: Verify decides whether a job succeeded

`verify` has been parsed and ignored since M1 — the old spec's accepted risk
#6. This is a **behavior change**, not a cleanup, which is why it is its own
change rather than riding along with a deletion.

Ordering matters and is not what the spec's one-line summary suggests: the
branch is published **whatever happens**, because "a job's branch always
survives" is the contract. `verify` runs after the commit and decides success,
so what it gates is *delivery* — the factory never opens a pull request on
work that does not build — not publication.

**Files:**
- Create: `tests/verify.rs`
- Modify: `src/scheduler.rs`, `src/event.rs`, `src/report.rs`,
  `src/state.rs`, `docs/superpowers/specs/2026-09-11-software-factory-v2.md`

**Interfaces:**
- Consumes: Task 5's `run_job`, `JobState`, `EventKind`.
- Produces: `EventKind::JobVerifyFailed { output: String }`, appended before
  `JobFailed` when `verify` exits non-zero. (Shipped as
  `JobVerifyFailed { reason: String }` — see Step 3.)

- [ ] **Step 1: Write the failing test**

Create `tests/verify.rs`:

```rust
mod support;

use assembly_line::event::EventKind;

#[tokio::test]
async fn a_job_whose_verify_fails_is_a_failed_job() {
    let h = support::Harness::with_config("verify = \"exit 1\"").await;

    let outcome = h.run_job("write a file").await;

    assert!(!outcome.succeeded, "a failing verify fails the job");
    assert!(
        outcome.has(|e| matches!(e, EventKind::JobVerifyFailed { .. })),
        "the failure should say verify was what rejected it"
    );
    assert!(
        outcome.has(|e| matches!(e, EventKind::JobBranchPublished { .. })),
        "the branch survives a failed verify — that is what makes it inspectable"
    );
}

#[tokio::test]
async fn a_job_whose_verify_passes_succeeds() {
    let h = support::Harness::with_config("verify = \"exit 0\"").await;

    let outcome = h.run_job("write a file").await;

    assert!(outcome.succeeded);
    assert!(!outcome.has(|e| matches!(e, EventKind::JobVerifyFailed { .. })));
}

#[tokio::test]
async fn a_job_with_no_verify_succeeds_on_the_agents_exit_code() {
    let h = support::Harness::with_config("").await;

    let outcome = h.run_job("write a file").await;

    assert!(outcome.succeeded);
}
```

- [ ] **Step 2: Run it to make sure it fails**

Run: `cargo test --test verify`
Expected: FAIL — `JobVerifyFailed` does not exist, and the first case passes a
job it should fail.

- [ ] **Step 3: Add the event**

In `src/event.rs`:

```rust
    /// `verify` rejected the work. Recorded before [`EventKind::JobFailed`],
    /// so a reader can tell a rejected job from one whose agent crashed.
    JobVerifyFailed { output: String },
```

(The field shipped as `reason: String`, not `output` — see `src/event.rs`.)

Add it to the progress-marker arm in `JobState::apply` — the transition to
`Failed` comes from the `JobFailed` that follows it — and to the exhaustive
match in `src/report.rs`, where it sets `detail`.

- [ ] **Step 4: Run verify inside the job**

In `src/scheduler.rs`, between the commit and the publish. `run_shell` already
runs a command in a directory with a timeout, writing into a log:

```rust
/// Whether `verify` accepts what the agent left. A job with no `verify`
/// succeeds on the agent's exit code alone.
///
/// Runs in the job's own checkout, after the commit, so it judges exactly the
/// tree the branch carries.
async fn verify_rejected_the_work(
    verify: Option<&str>,
    workspace: &Path,
    log_path: &Path,
    timeout: Option<Duration>,
    cancel: CancellationToken,
) -> anyhow::Result<Option<String>> {
    let Some(command) = verify else {
        return Ok(None);
    };

    Ok(run_shell(command, workspace, log_path, timeout, cancel)
        .await?
        .failure_reason())
}
```

In `run_job`, after `workspace::commit` and before `workspace::discard`:

```rust
    let rejected = verify_rejected_the_work(
        config.verify.as_deref(),
        &ws.path,
        log_path,
        timeout,
        cancel.clone(),
    )
    .await?;
```

Then fold it into the result: an agent failure still wins (it is the earlier
and more fundamental one), and a verify rejection turns an otherwise
successful job into `NodeResult::Failed` carrying its work. Emit
`JobVerifyFailed { output }` before `JobFailed` in `events_for_completion`.

- [ ] **Step 5: Run it to verify it passes**

Run: `cargo test --test verify`
Expected: PASS, all three cases.

- [ ] **Step 6: Correct the spec**

In `docs/superpowers/specs/2026-09-11-software-factory-v2.md`, the
Verification section says `verify` runs "before pushing". That reads as
gating publication, which would contradict the job contract. Change that
bullet to:

```markdown
- **`verify`, in-job, after the commit.** A fast local filter so the factory
  never *delivers* a pull request on code that does not compile, and the
  signal the tester loop iterates against without paying for a CI cycle per
  round. The branch is published either way — a rejected job is exactly the
  case where the diff is worth reading.
```

- [ ] **Step 7: Run the gate**

Run: `just check`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
jj describe -m "feat(job): verify decides whether a job succeeded

Parsed and ignored since M1 (old spec, accepted risk 6). It runs after the
commit and before delivery, so the branch survives a rejection and stays
inspectable — what verify gates is the pull request, not the push.

Tests: <N> (was <M>)."
jj new
```

---

### Task 7: Delivery opens a pull request for a job's branch

Task 3 deleted `deliver_if_complete` along with the run branch. Delivery
returns, pointed at the job's own branch, so F1 ends with a tool that produces
what the factory produces.

**Files:**
- Modify: `src/delivery.rs`, `src/main.rs`, `src/config.rs`
- Test: `tests/delivery.rs`

**Interfaces:**
- Consumes: Task 6's `run_job` result, `JobMeta::branch`.
- Produces: `delivery::deliver(repo, delivery, remote, job_branch, base) ->
  anyhow::Result<Delivered>` — unchanged signature, called with the job's
  branch. `DeliveryMode` has only `Pr` and `None`.

- [ ] **Step 1: Write the failing test**

In `tests/delivery.rs`, replace the `push` cases with one asserting a failed
job is not delivered:

```rust
#[tokio::test]
async fn a_failed_job_is_not_delivered() {
    let h = support::Harness::with_config("verify = \"exit 1\"").await;
    h.with_origin().await;

    let outcome = h.run_job("write a file").await;

    assert!(!outcome.succeeded);
    assert!(
        outcome.stdout.contains("not delivered"),
        "a job that did not pass verify should print its branch, not open a PR: {}",
        outcome.stdout
    );
}
```

- [ ] **Step 2: Run it to make sure it fails**

Run: `cargo test --test delivery a_failed_job_is_not_delivered`
Expected: FAIL — nothing calls `deliver` yet.

- [ ] **Step 3: Delete the push mode**

In `src/config.rs`, remove `DeliveryMode::Push`. Auto-merge by label replaces
it in F5, and a mode nothing reaches is dead weight in between. In
`src/delivery.rs`, delete the `Push` arm and `Delivered::LandedOn`.

- [ ] **Step 4: Call it from main**

```rust
/// Hand a finished job's branch on, once `verify` accepted it.
///
/// A failed job still leaves a real branch, but opening a pull request for
/// work that did not pass is noise — the branch name is printed instead, so
/// acting on it stays a decision rather than a default.
async fn deliver_if_verified(repo: &Path, config: &RepoConfig, meta: &JobMeta, failed: bool) {
    let Some(branch) = &meta.branch else {
        return; // The agent changed nothing, so there is nothing to deliver.
    };

    if failed {
        println!("branch: {branch} (not delivered — the job did not pass)");
        return;
    }

    let base = config
        .base
        .clone()
        .unwrap_or_else(|| meta.base_ref.clone());

    match delivery::deliver(
        repo,
        &config.delivery,
        assembly_line::workspace::DEFAULT_REMOTE,
        branch,
        &base,
    )
    .await
    {
        Ok(outcome) => println!("{outcome}"),
        Err(e) => eprintln!("warn: delivering {branch}: {e}"),
    }
}
```

Call it at the end of the `run` and `revise` handlers.

- [ ] **Step 5: Run it to verify it passes**

Run: `cargo test --test delivery`
Expected: PASS.

- [ ] **Step 6: Run the gate**

Run: `just check`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
jj describe -m "feat(delivery): a verified job opens a pull request

Delivery lost its target when the run branch went. It comes back pointed
at the job's own branch, gated on verify — a rejected job prints its
branch instead, because a pull request for work that does not pass is
noise.

Tests: <N> (was <M>)."
jj new
```

---

### Task 8: A demo worth having, and the docs

F1's last change: restore the `demo` recipe Task 2 deleted, now against a real
job, and mark the milestone.

**Files:**
- Modify: `justfile`, `README` if one exists (check with `ls`),
  `docs/superpowers/specs/2026-09-11-software-factory-v2.md`,
  `CLAUDE.md`
- Create: nothing

**Interfaces:**
- Consumes: everything above.
- Produces: nothing code-facing.

- [ ] **Step 1: Restore the demo against a real job**

In `justfile`, add a recipe that builds a throwaway repo with a config and a
fake agent, then runs one job:

```make
# Run one job end to end against a throwaway repo, to see real output.
demo:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --quiet
    bin="$CARGO_TARGET_DIR/debug/assembly"
    fake="$PWD/tests/fixtures/fake-agent.sh"
    dir=$(mktemp -d)
    cd "$dir"
    git init -q --initial-branch=main .
    git config user.email t@e.com && git config user.name T
    mkdir -p .assembly
    cat > .assembly/config.toml <<EOF
    provider = "fake"
    verify = "test -f agent-was-here"

    [providers.fake]
    cmd = "bash"
    args = ["$fake", "{prompt}", "demo"]
    EOF
    git add -A && git commit -qm "opt in to the factory"
    "$bin" run --prompt "make a change" || true
    echo
    "$bin" status
    echo "demo job left in $dir"
```

Confirm the `verify` command matches what `tests/fixtures/fake-agent.sh`
actually writes — read the fixture first and adjust the filename.

- [ ] **Step 2: Run the demo**

Run: `just demo`
Expected: a job runs, `verify` passes, `status` prints one line naming the
branch.

- [ ] **Step 3: Update CLAUDE.md's pointers**

`CLAUDE.md` still names the M1-era spec and the M2 plan. Change the two bullets
at the top to:

```markdown
- Design decisions: `docs/superpowers/specs/2026-09-11-software-factory-v2.md`
- Current milestone: `docs/superpowers/plans/2026-09-11-software-factory-f1.md`
```

Also update the **Style** section's two examples, which use `TaskKind` and a
`tasks` list that no longer exist — replace them with equivalents drawn from
the code as it now stands.

- [ ] **Step 4: Mark the milestone**

In the spec, change the F1 row of the Milestones table to `**F1** ✅` and add
to the plan's header `**Status:** shipped <date>`.

- [ ] **Step 5: Verify the whole stack**

Run: `just verify-stack`
Expected: every revision `ok`. If a revision fails, fix it with
`jj edit <rev>` — never by rewriting history above it.

- [ ] **Step 6: Commit**

```bash
jj describe -m "docs: F1 shipped

Tests: <N> (was <M> at the start of F1)."
jj new
```

---

## Self-review notes

Checked against the spec, with three things called out rather than hidden:

1. **`verify`'s placement contradicts the spec's own wording.** The spec says
   "before pushing"; the job contract says the branch always survives. Task 6
   resolves it — verify runs after the commit and gates *delivery* — and
   corrects the spec line in the same change.

2. **Delivery is deleted in Task 3 and restored in Task 7.** That is
   deliberate: carrying a dead `meta.run_branch` path through four changes
   would make each one lie about what works. The cost is that revisions 3–6
   produce a branch and no pull request.

3. **Two spec items are correctly absent from F1.** Job state moving under a
   daemon root belongs to F3, and `retries` belongs to the tester loop in F5 —
   so `retries` is deleted here rather than kept dangling. Branch pruning stays
   deferred, as the spec records.
