# assembly-line M2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `kind = "agent"` nodes actually execute. Each runs a configured provider command in its own git worktree, its work is committed and merged into a run branch, and the target repository's working tree is never modified.

**Architecture:** At run start, if the graph contains any agent node, the scheduler creates a run branch `al/run-<id>` off HEAD and checks it out into an *integration worktree* outside the repo. Every node then runs against that worktree — shell nodes directly in it, agent nodes each in their own worktree branched from the run branch. When an agent exits zero, its work is committed and merged into the run branch. Merges are serialized behind a mutex because they all target the one integration worktree.

**Tech Stack:** Unchanged. `src/git.rs` (already built in M1's follow-on) provides every git operation needed; no new dependencies.

**Spec:** `docs/superpowers/specs/2026-08-15-assembly-line.md`

**Prior plan:** `docs/superpowers/plans/2026-08-15-assembly-line-m1.md`

## Global Constraints

Read `CLAUDE.md` first — it is binding. In particular:

- **Functional style.** Iterator chains over accumulating `for` loops; `match` on the shape of data over if-else chains. The scheduler's await-loop is the documented exception.
- **Naming.** Bare verbs and nouns are rejected. Predicates read as claims (`dependency_is_satisfied`), mutators name the transition and its scope (`mark_pending_as_skipped`), constructors name the source (`RunReport::from_events`).
- **No speculative traits.** Do **not** introduce an `AgentRunner` trait in M2. A containerized agent is expressible today as a provider whose `cmd` is `docker`; the trait waits until a second implementor genuinely exists. This was decided explicitly — see the spec's "Isolation: three separable questions".
- **Clippy pedantic is enforced** via `[lints.clippy]` in `Cargo.toml`. `just check` must be clean. Do not add lint allows without a stated reason.
- **Every change in the jj stack must build, test, lint, and format on its own.** Verify with `just verify-stack`, not by hand.
- The event log is append-only. `RunState::apply` must stay a pure function of the event stream.
- The target repository is never modified: no commits on the user's branch, no changes to their working tree, no writes to their `.git/config` or `.git/info/exclude`.
- Every test must pass offline with no credentials and no real agent. Agents are faked with shell scripts.

## Decisions already made — do not relitigate

| Question | Decision |
|---|---|
| Node branch naming | Flat siblings: `al/run-42-impl-auth`. Git refs cannot nest under `al/run-42`, and there is a test asserting this. |
| Where worktrees live | `~/.assembly/wt/<run-id>/<node>/`, outside the repo. Removed on success, **kept on failure** for inspection. |
| Keeping seeded files out of commits | `git::commit_all_except`. `info/exclude` does not work per-worktree — git reads it from the common dir, and writing there would modify the user's repo. |
| Where merges happen | An integration worktree holding the run branch, so the user's checkout is never a merge target. |
| Retries, `verify`, gates, conflict resolution | **M3.** Not in this milestone. A conflict fails the node. |

---

## File Structure

- `src/workspace.rs` **(new)** — the lifecycle of one node's worktree: create, seed `copy` files, commit, tear down. This is where "what does a node's sandbox look like" lives.
- `src/provider.rs` **(new)** — turning a `Provider` config plus a prompt into an executable command. Small and pure, so it is testable without spawning anything.
- `src/exec.rs` — gains `run_command`, an argv-style sibling of `run_shell`. Agent commands must not go through `sh -c`, or a prompt containing quotes breaks the invocation.
- `src/paths.rs` — gains the worktree root under `$HOME`, and `RunMeta` gains the run branch and base sha.
- `src/event.rs` — gains commit/merge/conflict events.
- `src/scheduler.rs` — gains agent-node execution and merge serialization.
- `src/git.rs`, `src/dag.rs`, `src/config.rs`, `src/state.rs`, `src/report.rs` — small additions only.
- `tests/fixtures/` **(new)** — fake agent scripts.
- `tests/agent_nodes.rs`, `tests/workspace.rs`, `tests/provider.rs` **(new)**.

---

### Task 1: Provider command rendering

**Files:**
- Create: `src/provider.rs`
- Modify: `src/lib.rs`
- Test: `tests/provider.rs`

**Interfaces:**
- Consumes: `config::Provider`.
- Produces:
  - `provider::CommandSpec { pub program: String, pub args: Vec<String> }`
  - `fn provider::render_command(provider: &Provider, prompt: &str) -> CommandSpec`

**Why argv, not a shell string:** a prompt routinely contains quotes, newlines, and `$`. Substituting it into a shell string is an injection bug waiting to happen. `{prompt}` is replaced *inside an argument*, and the argument vector is passed to the OS directly.

- [ ] **Step 1: Write the failing test**

Create `tests/provider.rs`:

```rust
use assembly_line::config::Provider;
use assembly_line::provider::render_command;

fn provider(cmd: &str, args: &[&str]) -> Provider {
    Provider {
        cmd: cmd.to_string(),
        args: args.iter().map(|a| (*a).to_string()).collect(),
        adapter: None,
    }
}

#[test]
fn substitutes_the_prompt_into_the_argument_that_contains_it() {
    let spec = render_command(
        &provider("claude", &["-p", "{prompt}", "--permission-mode", "acceptEdits"]),
        "build the thing",
    );

    assert_eq!(spec.program, "claude");
    assert_eq!(
        spec.args,
        vec!["-p", "build the thing", "--permission-mode", "acceptEdits"]
    );
}

#[test]
fn a_prompt_with_shell_metacharacters_stays_one_argument() {
    let nasty = "fix `rm -rf /`; and \"quote\" $HOME\nsecond line";
    let spec = render_command(&provider("agent", &["--message", "{prompt}"]), nasty);

    assert_eq!(spec.args.len(), 2);
    assert_eq!(spec.args[1], nasty);
}

#[test]
fn the_placeholder_is_replaced_wherever_it_appears_in_an_argument() {
    let spec = render_command(&provider("agent", &["--task=({prompt})"]), "hi");
    assert_eq!(spec.args, vec!["--task=(hi)"]);
}

#[test]
fn every_occurrence_is_replaced() {
    let spec = render_command(&provider("agent", &["{prompt}", "{prompt}"]), "x");
    assert_eq!(spec.args, vec!["x", "x"]);
}

#[test]
fn a_provider_without_the_placeholder_is_rendered_unchanged() {
    let spec = render_command(&provider("agent", &["--stdin"]), "ignored");
    assert_eq!(spec.args, vec!["--stdin"]);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --test provider`
Expected: FAIL to compile — `unresolved import assembly_line::provider`.

- [ ] **Step 3: Write the implementation**

Create `src/provider.rs`:

```rust
//! Turning provider configuration into an executable command.

use crate::config::Provider;

/// The placeholder a provider's `args` use to receive the node's prompt.
pub const PROMPT_PLACEHOLDER: &str = "{prompt}";

/// A command as the OS takes it: a program and an argument vector.
///
/// Deliberately not a shell string. A prompt contains quotes, newlines and
/// `$`; substituting it into a shell string would be an injection bug.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
}

#[must_use]
pub fn render_command(provider: &Provider, prompt: &str) -> CommandSpec {
    CommandSpec {
        program: provider.cmd.clone(),
        args: provider
            .args
            .iter()
            .map(|arg| arg.replace(PROMPT_PLACEHOLDER, prompt))
            .collect(),
    }
}
```

Add to `src/lib.rs`, in alphabetical position:

```rust
pub mod provider;
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --test provider`
Expected: PASS, 5 tests.

- [ ] **Step 5: Commit as its own change**

```bash
just check
JJ_EDITOR=true jj describe -m "feat(provider): render a provider command with the node's prompt"
JJ_EDITOR=true jj new
```

---

### Task 2: Argv-style command execution

**Files:**
- Modify: `src/exec.rs`
- Test: `tests/exec.rs` (extend)

**Interfaces:**
- Consumes: `provider::CommandSpec`.
- Produces: `async fn exec::run_command(spec: &CommandSpec, cwd: impl AsRef<Path>, log_path: impl AsRef<Path>, timeout: Option<Duration>, cancel: CancellationToken) -> anyhow::Result<ShellOutcome>`

Return type is the existing `ShellOutcome`, so the scheduler treats agent and shell completions identically.

**Refactor note:** `run_shell` and `run_command` differ only in how the child is constructed. Extract the shared spawn-and-await logic rather than duplicating the `tokio::select!` block — a second copy of the timeout and cancellation handling will drift.

- [ ] **Step 1: Write the failing test**

Append to `tests/exec.rs`:

```rust
use assembly_line::exec::run_command;
use assembly_line::provider::CommandSpec;

fn spec(program: &str, args: &[&str]) -> CommandSpec {
    CommandSpec {
        program: program.to_string(),
        args: args.iter().map(|a| (*a).to_string()).collect(),
    }
}

#[tokio::test]
async fn run_command_captures_output_and_exit_code() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("node.log");

    let outcome = run_command(
        &spec("sh", &["-c", "echo hello; echo oops 1>&2; exit 2"]),
        tmp.path(),
        &log,
        None,
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(outcome, ShellOutcome::Exited(2));
    let body = std::fs::read_to_string(&log).unwrap();
    assert!(body.contains("hello") && body.contains("oops"), "{body}");
}

#[tokio::test]
async fn run_command_passes_arguments_without_shell_interpretation() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("node.log");

    // If this went through a shell, the backticks and `$HOME` would expand
    // and the semicolon would split the command.
    let literal = "a `b` ; c $HOME";
    run_command(
        &spec("printf", &["%s", literal]),
        tmp.path(),
        &log,
        None,
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(std::fs::read_to_string(&log).unwrap(), literal);
}

#[tokio::test]
async fn run_command_honours_the_timeout() {
    let tmp = tempfile::tempdir().unwrap();
    let outcome = run_command(
        &spec("sleep", &["30"]),
        tmp.path(),
        tmp.path().join("l.log"),
        Some(Duration::from_millis(150)),
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(outcome, ShellOutcome::TimedOut);
}

#[tokio::test]
async fn run_command_reports_a_missing_program_as_an_error_not_an_exit_code() {
    let tmp = tempfile::tempdir().unwrap();
    let err = run_command(
        &spec("definitely-not-a-real-program-xyz", &[]),
        tmp.path(),
        tmp.path().join("l.log"),
        None,
        CancellationToken::new(),
    )
    .await
    .unwrap_err()
    .to_string();

    assert!(
        err.contains("definitely-not-a-real-program-xyz"),
        "the error must name the program so a bad provider config is obvious: {err}"
    );
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --test exec`
Expected: FAIL to compile — `run_command` is undefined.

- [ ] **Step 3: Write the implementation**

In `src/exec.rs`, replace the body of `run_shell` and add `run_command`, sharing one supervision path:

```rust
use crate::provider::CommandSpec;

/// Run a command under `sh -c`. Used for `run` and `verify`, where the user
/// wrote a shell line and expects pipes and redirection to work.
///
/// The child gets two independent append-mode descriptors on the same file, so
/// output goes straight through the kernel and survives a kill — nothing is
/// buffered in this process and there are no reader tasks to drain.
///
/// # Errors
///
/// Returns an error if the log file cannot be opened or `sh` cannot be
/// spawned. A command that runs and fails is *not* an error — that is a
/// `ShellOutcome`, because a failing node is a normal part of a run.
pub async fn run_shell(
    cmd: &str,
    cwd: impl AsRef<Path>,
    log_path: impl AsRef<Path>,
    timeout: Option<Duration>,
    cancel: CancellationToken,
) -> anyhow::Result<ShellOutcome> {
    let mut command = Command::new("sh");
    command.arg("-c").arg(cmd);
    supervise(command, cmd, cwd, log_path, timeout, cancel).await
}

/// Run a program with an explicit argument vector, bypassing the shell.
///
/// Agent commands take this path: a prompt contains quotes, newlines and `$`,
/// and must reach the program as one argument rather than being re-parsed.
///
/// # Errors
///
/// Returns an error if the log file cannot be opened or the program cannot be
/// spawned — a missing binary means the provider config is wrong, which is
/// worth distinguishing from an agent that ran and failed.
pub async fn run_command(
    spec: &CommandSpec,
    cwd: impl AsRef<Path>,
    log_path: impl AsRef<Path>,
    timeout: Option<Duration>,
    cancel: CancellationToken,
) -> anyhow::Result<ShellOutcome> {
    let mut command = Command::new(&spec.program);
    command.args(&spec.args);
    supervise(command, &spec.program, cwd, log_path, timeout, cancel).await
}

/// Spawn `command`, then wait for whichever comes first: exit, deadline, or
/// cancellation. Shared so the timeout and kill semantics cannot drift between
/// the two entry points.
async fn supervise(
    mut command: Command,
    described_as: &str,
    cwd: impl AsRef<Path>,
    log_path: impl AsRef<Path>,
    timeout: Option<Duration>,
    cancel: CancellationToken,
) -> anyhow::Result<ShellOutcome> {
    let log_path = log_path.as_ref();

    let mut child = command
        .current_dir(cwd.as_ref())
        .stdin(Stdio::null())
        .stdout(Stdio::from(open_log_for_append(log_path)?))
        .stderr(Stdio::from(open_log_for_append(log_path)?))
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| anyhow::anyhow!("spawning `{described_as}`: {e}"))?;

    let deadline = async {
        match timeout {
            Some(d) => tokio::time::sleep(d).await,
            None => std::future::pending::<()>().await,
        }
    };

    tokio::select! {
        status = child.wait() => Ok(ShellOutcome::Exited(status?.code().unwrap_or(-1))),
        () = deadline => {
            let _ = child.kill().await;
            Ok(ShellOutcome::TimedOut)
        }
        () = cancel.cancelled() => {
            let _ = child.kill().await;
            Ok(ShellOutcome::Cancelled)
        }
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --test exec`
Expected: PASS, 11 tests (7 existing + 4 new).

- [ ] **Step 5: Commit**

```bash
just check
JJ_EDITOR=true jj describe -m "feat(exec): run a program with an explicit argument vector"
JJ_EDITOR=true jj new
```

---

### Task 3: Worktree paths and run metadata

**Files:**
- Modify: `src/paths.rs`
- Test: `tests/paths.rs` (extend)

**Interfaces:**
- Produces:
  - `fn paths::worktree_root(run_id: u64) -> Option<PathBuf>` — `~/.assembly/wt/<run-id>`, `None` when `$HOME` is unset
  - `RunPaths` gains `pub fn node_worktree(&self, node: &str) -> Option<PathBuf>` and `pub fn integration_worktree(&self) -> Option<PathBuf>`
  - `RunMeta` gains `pub run_branch: Option<String>` and `pub base_sha: Option<String>`

The integration worktree is named `_integration`. A leading underscore cannot collide with a task id, because ids match `^[A-Za-z0-9_-]+$`... **which does permit a leading underscore.** Guard it: add `ValidationError::ReservedId` for any id starting with `_`, so a task can never claim the integration directory.

`RunMeta`'s new fields are `Option` so existing `meta.json` files (written by M1) still deserialize. Add `#[serde(default)]`.

- [ ] **Step 1: Write the failing test**

Append to `tests/paths.rs`:

```rust
use assembly_line::paths::worktree_root;

#[test]
fn worktrees_live_under_home_not_in_the_repo() {
    let root = worktree_root(42).expect("HOME is set in the test environment");
    assert!(root.ends_with(".assembly/wt/42"), "{}", root.display());
    assert!(
        root.starts_with(std::env::var("HOME").unwrap()),
        "{}",
        root.display()
    );
}

#[test]
fn node_and_integration_worktrees_are_siblings() {
    let tmp = tempfile::tempdir().unwrap();
    let run = create_run(&runs_root(tmp.path()), 7).unwrap();

    let node = run.node_worktree("impl-auth").unwrap();
    let integration = run.integration_worktree().unwrap();

    assert_eq!(node.parent(), integration.parent());
    assert!(node.ends_with("impl-auth"));
    assert!(integration.ends_with("_integration"));
}

#[test]
fn meta_without_a_run_branch_still_deserializes() {
    // An M1 meta.json has no branch fields; resume must not choke on it.
    let tmp = tempfile::tempdir().unwrap();
    let run = create_run(&runs_root(tmp.path()), 1).unwrap();
    std::fs::write(run.meta(), r#"{"graph":"g.toml","jobs":4}"#).unwrap();

    let meta = read_meta(&run).unwrap();
    assert_eq!(meta.jobs, 4);
    assert!(meta.run_branch.is_none());
    assert!(meta.base_sha.is_none());
}

#[test]
fn meta_round_trips_the_run_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let run = create_run(&runs_root(tmp.path()), 1).unwrap();
    write_meta(
        &run,
        &RunMeta {
            graph: "g.toml".into(),
            jobs: 2,
            run_branch: Some("al/run-1".into()),
            base_sha: Some("abc123".into()),
        },
    )
    .unwrap();

    let back = read_meta(&run).unwrap();
    assert_eq!(back.run_branch.as_deref(), Some("al/run-1"));
    assert_eq!(back.base_sha.as_deref(), Some("abc123"));
}
```

Add to `tests/validate.rs`:

```rust
#[test]
fn rejects_an_id_that_would_collide_with_the_integration_worktree() {
    let src = "[[task]]\nid=\"_integration\"\nkind=\"shell\"\nrun=\"true\"\n";
    let errs = Dag::build(&tasks(src)).unwrap_err();
    assert!(
        errs.contains(&ValidationError::ReservedId("_integration".into())),
        "{errs:?}"
    );
}
```

Existing `RunMeta` constructions in `tests/resume.rs` and `src/main.rs` must gain the two new fields; update them in this task so the suite compiles.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test paths --test validate`
Expected: FAIL to compile — `worktree_root`, `node_worktree`, `ReservedId` undefined.

- [ ] **Step 3: Write the implementation**

In `src/paths.rs`:

```rust
/// Worktrees live under `$HOME`, never inside the repository — the target repo
/// must stay untouched, and a worktree inside it would need a `.gitignore`
/// entry we are not entitled to add.
///
/// `None` when `$HOME` is unset, which the caller should report rather than
/// guessing a location.
#[must_use]
pub fn worktree_root(run_id: u64) -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join(".assembly")
            .join("wt")
            .join(run_id.to_string())
    })
}

/// Directory name for the worktree holding the run branch. Task ids may not
/// start with `_`, so this cannot collide with a node.
pub const INTEGRATION_WORKTREE: &str = "_integration";
```

and on `RunPaths`:

```rust
    #[must_use]
    pub fn node_worktree(&self, node: &str) -> Option<PathBuf> {
        worktree_root(self.id).map(|root| root.join(node))
    }

    #[must_use]
    pub fn integration_worktree(&self) -> Option<PathBuf> {
        worktree_root(self.id).map(|root| root.join(INTEGRATION_WORKTREE))
    }
```

and extend `RunMeta`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunMeta {
    /// Path to the graph file, exactly as given on the command line.
    pub graph: PathBuf,
    pub jobs: usize,
    /// Set once the run creates a branch, which only happens when the graph
    /// contains at least one agent node.
    #[serde(default)]
    pub run_branch: Option<String>,
    #[serde(default)]
    pub base_sha: Option<String>,
}
```

In `src/dag.rs`, add the variant, its `Display` arm, and the check:

```rust
    ReservedId(String),
```

```rust
            Self::ReservedId(id) => write!(
                f,
                "task id '{id}' is reserved: ids may not start with '_', which assembly-line uses for its own worktrees"
            ),
```

In `id_naming_errors`, chain a third iterator:

```rust
    let reserved = tasks
        .iter()
        .filter(|t| t.id.starts_with('_'))
        .map(|t| ValidationError::ReservedId(t.id.clone()));

    invalid.chain(duplicated).chain(reserved)
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test`
Expected: PASS. Fix any `RunMeta` construction sites the compiler flags.

- [ ] **Step 5: Commit**

```bash
just check
JJ_EDITOR=true jj describe -m "feat(paths): worktree locations under HOME and run-branch metadata"
JJ_EDITOR=true jj new
```

---

### Task 4: Commit, merge, and conflict events

**Files:**
- Modify: `src/event.rs`, `src/state.rs`, `src/report.rs`
- Test: `tests/event_log.rs`, `tests/state.rs`, `tests/report.rs` (extend)

**Interfaces:**
- `EventKind` gains:
  - `RunBranchCreated { branch: String, base_sha: String }`
  - `NodeCommitted { node: String, sha: String, files: usize, insertions: usize, deletions: usize }`
  - `NodeMerged { node: String, sha: String }`
  - `NodeMergeConflicted { node: String, paths: Vec<String> }`
- `NodeReport` gains `pub diff: Option<DiffSummary>` where `DiffSummary { files, insertions, deletions }`.

**Critical:** none of the new events change `NodeState`. A node stays `Running` until `NodeFinished` or `NodeFailed`. Add them to `EventKind::node()` where applicable, and to `RunState::apply`'s match as explicit no-ops — do **not** add a `_ => {}` arm, or the next event type will be silently ignored.

- [ ] **Step 1: Write the failing test**

Append to `tests/state.rs`:

```rust
#[test]
fn commit_and_merge_events_do_not_change_node_state() {
    let g = parse_graph(DIAMOND).unwrap();
    let dag = Dag::build(&g.tasks).unwrap();
    let mut st = RunState::new(dag.ids());

    st.apply(&started("build"));
    st.apply(&EventKind::NodeCommitted {
        node: "build".into(),
        sha: "abc".into(),
        files: 2,
        insertions: 10,
        deletions: 1,
    });
    assert_eq!(st.state("build"), NodeState::Running, "a commit is not completion");

    st.apply(&EventKind::NodeMerged {
        node: "build".into(),
        sha: "def".into(),
    });
    assert_eq!(st.state("build"), NodeState::Running, "a merge is not completion");

    st.apply(&finished("build"));
    assert_eq!(st.state("build"), NodeState::Done);
}

#[test]
fn a_conflict_alone_does_not_fail_a_node() {
    let g = parse_graph(DIAMOND).unwrap();
    let dag = Dag::build(&g.tasks).unwrap();
    let mut st = RunState::new(dag.ids());

    st.apply(&started("build"));
    st.apply(&EventKind::NodeMergeConflicted {
        node: "build".into(),
        paths: vec!["src/lib.rs".into()],
    });
    assert_eq!(
        st.state("build"),
        NodeState::Running,
        "the scheduler decides; the conflict event only records what happened"
    );
}
```

Append to `tests/event_log.rs`:

```rust
#[test]
fn merge_events_round_trip_through_the_log() {
    let mut log = EventLog::new(Vec::new());
    log.append(EventKind::NodeMergeConflicted {
        node: "impl-api".into(),
        paths: vec!["src/routes.rs".into(), "src/lib.rs".into()],
    })
    .unwrap();

    let raw = String::from_utf8(log.sink().clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
    assert_eq!(v["t"], "node_merge_conflicted");
    assert_eq!(v["paths"][0], "src/routes.rs");
}
```

Append to `tests/report.rs`:

```rust
#[test]
fn a_node_reports_the_diff_it_committed() {
    let events = timeline(vec![
        (0, started("build", 1)),
        (
            1,
            EventKind::NodeCommitted {
                node: "build".into(),
                sha: "abc".into(),
                files: 3,
                insertions: 120,
                deletions: 4,
            },
        ),
        (
            2,
            EventKind::NodeFinished {
                node: "build".into(),
                exit_code: 0,
            },
        ),
    ]);

    let report = RunReport::from_events(1, &ids(), &events);
    let diff = report.nodes[0].diff.expect("a diff summary");
    assert_eq!((diff.files, diff.insertions, diff.deletions), (3, 120, 4));
    assert!(report.to_terminal_tree().contains("+120"));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test state --test event_log --test report`
Expected: FAIL to compile — the new variants do not exist.

- [ ] **Step 3: Write the implementation**

In `src/event.rs`, extend `EventKind` and `node()`:

```rust
    RunBranchCreated { branch: String, base_sha: String },
    NodeCommitted {
        node: String,
        sha: String,
        files: usize,
        insertions: usize,
        deletions: usize,
    },
    NodeMerged { node: String, sha: String },
    NodeMergeConflicted { node: String, paths: Vec<String> },
```

```rust
    pub fn node(&self) -> Option<&str> {
        match self {
            Self::NodeStarted { node, .. }
            | Self::NodeFinished { node, .. }
            | Self::NodeFailed { node, .. }
            | Self::NodeSkipped { node, .. }
            | Self::NodeCommitted { node, .. }
            | Self::NodeMerged { node, .. }
            | Self::NodeMergeConflicted { node, .. } => Some(node),
            Self::RunStarted { .. } | Self::RunFinished { .. } | Self::RunBranchCreated { .. } => {
                None
            }
        }
    }
```

In `src/state.rs`, extend the `apply` match with explicit no-ops and a comment:

```rust
            // Progress markers, not transitions: a node is Running until it
            // finishes or fails. Listed explicitly so a new event type is a
            // compile error rather than a silent omission.
            EventKind::RunBranchCreated { .. }
            | EventKind::NodeCommitted { .. }
            | EventKind::NodeMerged { .. }
            | EventKind::NodeMergeConflicted { .. } => None,
```

In `src/report.rs`, add `DiffSummary`, put it on `NodeProgress` and `NodeReport`, record it in `after_node_event` for `NodeCommitted`, and render it in the tree row (e.g. `+120/-4` in a new column). Keep `state_glyph` and column alignment intact.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
just check
JJ_EDITOR=true jj describe -m "feat(event): record commits, merges, and merge conflicts"
JJ_EDITOR=true jj new
```

---

### Task 5: The node workspace

**Files:**
- Create: `src/workspace.rs`
- Modify: `src/lib.rs`
- Test: `tests/workspace.rs`

**Interfaces:**
- Produces:
  - `workspace::NodeWorkspace { pub path: PathBuf, pub branch: String, pub seeded: Vec<String> }`
  - `async fn workspace::create(repo: impl AsRef<Path>, path: impl AsRef<Path>, branch: &str, base_sha: &str, seed_from: impl AsRef<Path>, copy_paths: &[String]) -> anyhow::Result<NodeWorkspace>`
  - `async fn workspace::commit(ws: &NodeWorkspace, message: &str) -> anyhow::Result<Option<String>>`
  - `async fn workspace::discard(repo: impl AsRef<Path>, ws: &NodeWorkspace) -> anyhow::Result<()>`
  - `fn workspace::run_branch_name(run_id: u64) -> String` → `al/run-<id>`
  - `fn workspace::node_branch_name(run_id: u64, node: &str) -> String` → `al/run-<id>-<node>`

`create` takes an explicit `path` and `branch` rather than deriving them from
a run id, so the scheduler owns naming (via `paths::` and the two
`*_branch_name` helpers) and this module stays a pure worktree mechanism.

`seed_from` is the directory `copy` paths resolve against — the CLI's working directory, so a cloned run behaves the same. `seeded` carries the relative paths, which `commit` hands to `git::commit_all_except`.

- [ ] **Step 1: Write the failing test**

Create `tests/workspace.rs`:

```rust
use assembly_line::git::{self, commit_all, head_sha};
use assembly_line::workspace::{self, node_branch_name, run_branch_name};
use std::path::PathBuf;

struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    seed: PathBuf,
    wt_root: PathBuf,
}

impl Fixture {
    async fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let seed = tmp.path().join("seed");
        let wt_root = tmp.path().join("wt");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&seed).unwrap();
        std::fs::create_dir_all(&wt_root).unwrap();

        for args in [
            vec!["init", "--initial-branch=main"],
            vec!["config", "user.email", "t@e.com"],
            vec!["config", "user.name", "T"],
            vec!["config", "commit.gpgsign", "false"],
        ] {
            git::run_allowing_failure(&repo, &args).await.unwrap();
        }
        std::fs::write(repo.join("README.md"), "base\n").unwrap();
        commit_all(&repo, "initial").await.unwrap().unwrap();

        Fixture { _tmp: tmp, repo, seed, wt_root }
    }
}

#[test]
fn branch_names_are_flat_siblings() {
    assert_eq!(run_branch_name(42), "al/run-42");
    assert_eq!(node_branch_name(42, "impl-auth"), "al/run-42-impl-auth");
    assert!(
        !node_branch_name(42, "impl-auth").starts_with(&format!("{}/", run_branch_name(42))),
        "a node branch must not nest under the run branch; git forbids it"
    );
}

#[tokio::test]
async fn creating_a_workspace_checks_out_the_base_commit() {
    let fx = Fixture::new().await;
    let base = head_sha(&fx.repo).await.unwrap();

    let ws = workspace::create(
        &fx.repo,
        &fx.wt_root.join("impl-auth"),
        "al/run-1-impl-auth",
        &base,
        &fx.seed,
        &[],
    )
    .await
    .unwrap();

    assert!(ws.path.join("README.md").is_file());
    assert_eq!(ws.branch, "al/run-1-impl-auth");
    assert!(ws.seeded.is_empty());
    assert!(!fx.repo.join("should-not-exist").exists());
}

#[tokio::test]
async fn seeded_files_are_copied_in_and_kept_out_of_the_commit() {
    let fx = Fixture::new().await;
    std::fs::write(fx.seed.join(".env"), "API_KEY=hunter2\n").unwrap();
    let base = head_sha(&fx.repo).await.unwrap();

    let ws = workspace::create(
        &fx.repo,
        &fx.wt_root.join("n"),
        "al/run-1-n",
        &base,
        &fx.seed,
        &[".env".to_string()],
    )
    .await
    .unwrap();

    assert_eq!(
        std::fs::read_to_string(ws.path.join(".env")).unwrap(),
        "API_KEY=hunter2\n",
        "the agent must be able to read it"
    );

    std::fs::write(ws.path.join("work.txt"), "did the work\n").unwrap();
    workspace::commit(&ws, "node work").await.unwrap().unwrap();

    let tracked = git::run_allowing_failure(&ws.path, &["ls-files"])
        .await
        .unwrap()
        .stdout;
    assert!(tracked.contains("work.txt"), "{tracked}");
    assert!(!tracked.contains(".env"), "seeded secret was committed: {tracked}");
}

#[tokio::test]
async fn seeding_preserves_nested_paths() {
    let fx = Fixture::new().await;
    std::fs::create_dir_all(fx.seed.join(".claude")).unwrap();
    std::fs::write(fx.seed.join(".claude/settings.local.json"), "{}\n").unwrap();
    let base = head_sha(&fx.repo).await.unwrap();

    let ws = workspace::create(
        &fx.repo,
        &fx.wt_root.join("n"),
        "al/run-1-n",
        &base,
        &fx.seed,
        &[".claude/settings.local.json".to_string()],
    )
    .await
    .unwrap();

    assert!(ws.path.join(".claude/settings.local.json").is_file());
}

#[tokio::test]
async fn a_missing_seed_path_names_the_file() {
    let fx = Fixture::new().await;
    let base = head_sha(&fx.repo).await.unwrap();

    let err = workspace::create(
        &fx.repo,
        &fx.wt_root.join("n"),
        "al/run-1-n",
        &base,
        &fx.seed,
        &["nope.env".to_string()],
    )
    .await
    .unwrap_err()
    .to_string();

    assert!(err.contains("nope.env"), "{err}");
}

#[tokio::test]
async fn committing_an_untouched_workspace_produces_nothing() {
    let fx = Fixture::new().await;
    let base = head_sha(&fx.repo).await.unwrap();
    let ws = workspace::create(&fx.repo, &fx.wt_root.join("n"), "al/run-1-n", &base, &fx.seed, &[])
        .await
        .unwrap();

    assert!(
        workspace::commit(&ws, "nothing happened").await.unwrap().is_none(),
        "an agent may correctly decide no change is needed"
    );
}

#[tokio::test]
async fn discarding_a_workspace_removes_it_but_keeps_the_branch() {
    let fx = Fixture::new().await;
    let base = head_sha(&fx.repo).await.unwrap();
    let ws = workspace::create(&fx.repo, &fx.wt_root.join("n"), "al/run-1-n", &base, &fx.seed, &[])
        .await
        .unwrap();

    workspace::discard(&fx.repo, &ws).await.unwrap();

    assert!(!ws.path.exists());
    assert!(
        git::branch_exists(&fx.repo, "al/run-1-n").await.unwrap(),
        "the branch is the record of the work; only the checkout is disposable"
    );
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --test workspace`
Expected: FAIL to compile — `unresolved import assembly_line::workspace`.

- [ ] **Step 3: Write the implementation**

Create `src/workspace.rs`. Keep it declarative; `create` is: validate seeds exist → add worktree → copy seeds → return.

```rust
//! One node's sandbox: a git worktree, optionally seeded with files the
//! repository does not carry.

use crate::git;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct NodeWorkspace {
    pub path: PathBuf,
    pub branch: String,
    /// Relative paths copied in, which must never reach a commit.
    pub seeded: Vec<String>,
}

#[must_use]
pub fn run_branch_name(run_id: u64) -> String {
    format!("al/run-{run_id}")
}

/// Node branches are flat siblings of the run branch. Git refs are paths, so
/// `al/run-42` and `al/run-42/node` cannot both exist.
#[must_use]
pub fn node_branch_name(run_id: u64, node: &str) -> String {
    format!("al/run-{run_id}-{node}")
}

/// Create a worktree for `branch` at `base_sha` and seed it.
///
/// # Errors
///
/// Returns an error if a declared seed path does not exist under `seed_from`,
/// if the worktree cannot be created, or if a copy fails. Seed paths are
/// checked before the worktree is made, so a typo leaves nothing behind.
pub async fn create(
    repo: impl AsRef<Path>,
    path: impl AsRef<Path>,
    branch: &str,
    base_sha: &str,
    seed_from: impl AsRef<Path>,
    copy_paths: &[String],
) -> anyhow::Result<NodeWorkspace> {
    let (repo, path, seed_from) = (repo.as_ref(), path.as_ref(), seed_from.as_ref());

    let missing: Vec<&String> = copy_paths
        .iter()
        .filter(|rel| !seed_from.join(rel).exists())
        .collect();
    if let Some(first) = missing.first() {
        anyhow::bail!(
            "copy path '{first}' does not exist under {}",
            seed_from.display()
        );
    }

    git::add_worktree(repo, path, branch, base_sha).await?;

    copy_paths.iter().try_for_each(|rel| -> anyhow::Result<()> {
        let destination = path.join(rel);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(seed_from.join(rel), destination)
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!("copying '{rel}' into the workspace: {e}"))
    })?;

    Ok(NodeWorkspace {
        path: path.to_path_buf(),
        branch: branch.to_string(),
        seeded: copy_paths.to_vec(),
    })
}

/// Commit whatever the agent left, excluding seeded files.
///
/// # Errors
///
/// See [`git::commit_all_except`] — notably, an error if the agent committed a
/// seeded file itself.
pub async fn commit(ws: &NodeWorkspace, message: &str) -> anyhow::Result<Option<String>> {
    git::commit_all_except(&ws.path, message, &ws.seeded).await
}

/// Remove the checkout. The branch survives, because it is the record of what
/// the node did.
///
/// # Errors
///
/// Returns an error if git cannot remove the worktree.
pub async fn discard(repo: impl AsRef<Path>, ws: &NodeWorkspace) -> anyhow::Result<()> {
    git::remove_worktree(repo, &ws.path).await
}
```

Add `pub mod workspace;` to `src/lib.rs`.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --test workspace`
Expected: PASS, 8 tests.

- [ ] **Step 5: Commit**

```bash
just check
JJ_EDITOR=true jj describe -m "feat(workspace): per-node git worktrees seeded with declared files"
JJ_EDITOR=true jj new
```

---

### Task 6: Agent nodes execute, commit, and merge

**Files:**
- Modify: `src/scheduler.rs`, `src/main.rs`
- Create: `tests/fixtures/fake-agent.sh`, `tests/fixtures/failing-agent.sh`, `tests/fixtures/noop-agent.sh`
- Test: `tests/agent_nodes.rs`

**Interfaces:**
- `RunOpts` gains:
  ```rust
  pub struct RunOpts {
      pub jobs: usize,
      pub cwd: PathBuf,
      pub cancel: CancellationToken,
      /// The repository worktrees are created from. `None` disables agent
      /// nodes, which is how shell-only runs keep M1 behaviour exactly.
      pub repo: Option<PathBuf>,
      /// Where `copy` paths resolve from — the CLI's working directory.
      pub seed_from: PathBuf,
  }
  ```
- `scheduler::AGENT_UNSUPPORTED` is **removed**; delete the M1 test that asserts it and replace it with the real behaviour tests below.

**Execution order for one agent node**

1. Create workspace at the *current run branch tip* (re-read before each node, so a node sees earlier merges).
2. Render the provider command; run it in the workspace with the node's `max_duration`.
3. Non-zero exit, timeout, or cancellation → `NodeFailed`, keep the worktree.
4. Zero exit → `workspace::commit`. `None` (no changes) → `NodeFinished` with no merge.
5. Some(sha) → `NodeCommitted` with the diff stat, then take the **merge lock** and merge into the integration worktree.
6. `Merged`/`AlreadyUpToDate` → `NodeMerged` then `NodeFinished`, and discard the worktree.
7. `Conflicted` → `git::abort_merge`, `NodeMergeConflicted`, then `NodeFailed`, keeping the worktree.

**Merge serialization:** all merges target one integration worktree, so wrap it in a `tokio::sync::Mutex`. Acquire it *only* around the merge, never while an agent is running, or the run becomes serial.

**Known limitation to record, not fix:** a merge lands in the integration worktree while shell nodes may be running there. M3 routes merges through the approval queue, which serializes them against everything else. Note this in the spec under "Accepted risks"; do not attempt a fix here.

- [ ] **Step 1: Write the fake agents**

Create `tests/fixtures/fake-agent.sh` (mark executable, `chmod +x`):

```bash
#!/usr/bin/env bash
# A fake coding agent: writes a file named after its prompt and exits 0.
# Never touches the network. $1 is the prompt.
set -euo pipefail
echo "fake-agent: $1"
printf '%s\n' "$1" > agent-output.txt
```

Create `tests/fixtures/failing-agent.sh`:

```bash
#!/usr/bin/env bash
# Writes a partial change, then fails — the worktree must be kept.
set -euo pipefail
echo "fake-agent: starting work on $1"
printf 'half done\n' > partial.txt
echo "fake-agent: giving up" 1>&2
exit 3
```

Create `tests/fixtures/noop-agent.sh`:

```bash
#!/usr/bin/env bash
# Decides no change is needed. A legitimate outcome, not a failure.
set -euo pipefail
echo "fake-agent: nothing to do for $1"
```

Create `tests/fixtures/conflicting-agent.sh`:

```bash
#!/usr/bin/env bash
# Every instance writes the same file with different content, so the second
# merge into the run branch conflicts. $2 distinguishes them.
set -euo pipefail
printf 'written by %s\n' "${2:-unknown}" > shared.txt
```

- [ ] **Step 2: Write the failing test**

Create `tests/agent_nodes.rs`:

```rust
use assembly_line::config::parse_graph;
use assembly_line::dag::Dag;
use assembly_line::event::{EventKind, EventLog, RunStatus};
use assembly_line::git::{self, commit_all, head_sha};
use assembly_line::paths::{create_run, runs_root};
use assembly_line::scheduler::{RunOpts, execute};
use assembly_line::state::{NodeState, RunState};
use assembly_line::workspace::run_branch_name;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

struct Harness {
    tmp: tempfile::TempDir,
    repo: PathBuf,
}

struct Outcome {
    status: RunStatus,
    state: RunState,
    events: Vec<EventKind>,
    run_id: u64,
}

impl Outcome {
    fn has(&self, predicate: impl Fn(&EventKind) -> bool) -> bool {
        self.events.iter().any(predicate)
    }
}

impl Harness {
    async fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [
            vec!["init", "--initial-branch=main"],
            vec!["config", "user.email", "t@e.com"],
            vec!["config", "user.name", "T"],
            vec!["config", "commit.gpgsign", "false"],
        ] {
            git::run_allowing_failure(&repo, &args).await.unwrap();
        }
        std::fs::write(repo.join("README.md"), "base\n").unwrap();
        commit_all(&repo, "initial").await.unwrap().unwrap();

        Harness { tmp, repo }
    }

    /// A provider that runs one of the fixture scripts.
    fn provider_block(&self, script: &str, extra_arg: &str) -> String {
        format!(
            "[providers.fake]\ncmd = \"bash\"\nargs = [\"{}\", \"{{prompt}}\", \"{extra_arg}\"]\n",
            fixture(script).display()
        )
    }

    async fn run(&self, src: &str, jobs: usize) -> Outcome {
        let graph = parse_graph(src).unwrap();
        let dag = Dag::build(&graph.tasks).unwrap();
        let run = create_run(&runs_root(self.tmp.path()), 1).unwrap();
        let mut log = EventLog::open_append(run.events()).unwrap();
        let mut state = RunState::new(dag.ids());

        let opts = RunOpts {
            jobs,
            cwd: self.repo.clone(),
            cancel: CancellationToken::new(),
            repo: Some(self.repo.clone()),
            seed_from: self.repo.clone(),
        };
        let status = execute(&graph, &dag, &run, &mut log, &mut state, &opts)
            .await
            .unwrap();

        let events = EventLog::read(run.events())
            .unwrap()
            .into_iter()
            .map(|e| e.kind)
            .collect();

        Outcome { status, state, events, run_id: run.id }
    }
}

#[tokio::test]
async fn an_agent_node_runs_commits_and_merges_into_the_run_branch() {
    let h = Harness::new().await;
    let base = head_sha(&h.repo).await.unwrap();
    let src = format!(
        "{}\n[[task]]\nid = \"impl-auth\"\nkind = \"agent\"\nprovider = \"fake\"\n\
         prompt = \"add authentication\"\n",
        h.provider_block("fake-agent.sh", "a")
    );

    let out = h.run(&src, 1).await;

    assert_eq!(out.status, RunStatus::Ok);
    assert_eq!(out.state.state("impl-auth"), NodeState::Done);
    assert!(out.has(|k| matches!(k, EventKind::NodeCommitted { node, .. } if node == "impl-auth")));
    assert!(out.has(|k| matches!(k, EventKind::NodeMerged { node, .. } if node == "impl-auth")));

    // The user's checkout and branch are untouched.
    assert_eq!(head_sha(&h.repo).await.unwrap(), base);
    assert_eq!(
        git::current_branch(&h.repo).await.unwrap().as_deref(),
        Some("main")
    );
    assert!(!h.repo.join("agent-output.txt").exists());

    // The work is on the run branch.
    let branch = run_branch_name(out.run_id);
    assert!(git::branch_exists(&h.repo, &branch).await.unwrap());
    let listed = git::run_allowing_failure(&h.repo, &["ls-tree", "--name-only", &branch])
        .await
        .unwrap()
        .stdout;
    assert!(listed.contains("agent-output.txt"), "{listed}");
}

#[tokio::test]
async fn the_prompt_reaches_the_agent_intact() {
    let h = Harness::new().await;
    let src = format!(
        "{}\n[[task]]\nid = \"n\"\nkind = \"agent\"\nprovider = \"fake\"\n\
         prompt = \"quotes \\\" and $HOME and ; semicolons\"\n",
        h.provider_block("fake-agent.sh", "a")
    );

    let out = h.run(&src, 1).await;
    assert_eq!(out.status, RunStatus::Ok);

    let content = git::run_allowing_failure(
        &h.repo,
        &["show", &format!("{}:agent-output.txt", run_branch_name(out.run_id))],
    )
    .await
    .unwrap()
    .stdout;
    assert!(content.contains("$HOME"), "the shell expanded the prompt: {content}");
    assert!(content.contains("; semicolons"), "{content}");
}

#[tokio::test]
async fn a_failing_agent_fails_the_node_and_keeps_its_worktree() {
    let h = Harness::new().await;
    let src = format!(
        "{}\n[[task]]\nid = \"broken\"\nkind = \"agent\"\nprovider = \"fake\"\nprompt = \"x\"\n",
        h.provider_block("failing-agent.sh", "a")
    );

    let out = h.run(&src, 1).await;

    assert_eq!(out.status, RunStatus::Partial);
    assert_eq!(out.state.state("broken"), NodeState::Failed);
    assert!(out.has(|k| matches!(k, EventKind::NodeFailed { reason, .. } if reason.contains("exit 3"))));
    assert!(!out.has(|k| matches!(k, EventKind::NodeMerged { .. })));
}

#[tokio::test]
async fn an_agent_that_changes_nothing_succeeds_without_a_merge() {
    let h = Harness::new().await;
    let src = format!(
        "{}\n[[task]]\nid = \"n\"\nkind = \"agent\"\nprovider = \"fake\"\nprompt = \"x\"\n",
        h.provider_block("noop-agent.sh", "a")
    );

    let out = h.run(&src, 1).await;

    assert_eq!(out.status, RunStatus::Ok);
    assert_eq!(out.state.state("n"), NodeState::Done);
    assert!(!out.has(|k| matches!(k, EventKind::NodeCommitted { .. })));
    assert!(!out.has(|k| matches!(k, EventKind::NodeMerged { .. })));
}

#[tokio::test]
async fn a_second_node_sees_the_first_nodes_merged_work() {
    let h = Harness::new().await;
    let src = format!(
        "{}\n\
         [[task]]\nid = \"first\"\nkind = \"agent\"\nprovider = \"fake\"\nprompt = \"one\"\n\
         [[task]]\nid = \"second\"\nkind = \"shell\"\nneeds = [\"first\"]\n\
         run = \"test -f agent-output.txt\"\n",
        h.provider_block("fake-agent.sh", "a")
    );

    let out = h.run(&src, 1).await;

    assert_eq!(
        out.state.state("second"),
        NodeState::Done,
        "a dependent must run against the merged run branch, not the pristine repo"
    );
    assert_eq!(out.status, RunStatus::Ok);
}

#[tokio::test]
async fn two_agents_editing_the_same_file_conflict_on_the_second_merge() {
    let h = Harness::new().await;
    let src = format!(
        "[providers.a]\ncmd = \"bash\"\nargs = [\"{script}\", \"{{prompt}}\", \"a\"]\n\
         [providers.b]\ncmd = \"bash\"\nargs = [\"{script}\", \"{{prompt}}\", \"b\"]\n\
         [[task]]\nid = \"one\"\nkind = \"agent\"\nprovider = \"a\"\nprompt = \"x\"\n\
         [[task]]\nid = \"two\"\nkind = \"agent\"\nprovider = \"b\"\nprompt = \"y\"\n",
        script = fixture("conflicting-agent.sh").display()
    );

    let out = h.run(&src, 2).await;

    assert_eq!(out.status, RunStatus::Partial);
    assert!(
        out.has(|k| matches!(k, EventKind::NodeMergeConflicted { paths, .. } if paths.contains(&"shared.txt".to_string()))),
        "{:?}",
        out.events
    );

    let done = [out.state.state("one"), out.state.state("two")];
    assert!(
        done.contains(&NodeState::Done) && done.contains(&NodeState::Failed),
        "exactly one should land and one should conflict: {done:?}"
    );
}

#[tokio::test]
async fn seeded_files_reach_the_agent_but_never_the_run_branch() {
    let h = Harness::new().await;
    std::fs::write(h.repo.join(".env"), "API_KEY=hunter2\n").unwrap();
    let src = format!(
        "{}\n[workspace]\ncopy = [\".env\"]\n\
         [[task]]\nid = \"n\"\nkind = \"agent\"\nprovider = \"fake\"\nprompt = \"x\"\n",
        h.provider_block("fake-agent.sh", "a")
    );

    let out = h.run(&src, 1).await;
    assert_eq!(out.status, RunStatus::Ok);

    let listed = git::run_allowing_failure(
        &h.repo,
        &["ls-tree", "--name-only", "-r", &run_branch_name(out.run_id)],
    )
    .await
    .unwrap()
    .stdout;
    assert!(!listed.contains(".env"), "the seeded secret reached a branch: {listed}");
}

#[tokio::test]
async fn a_shell_only_graph_creates_no_branch_and_no_worktrees() {
    let h = Harness::new().await;
    let out = h.run("[[task]]\nid = \"a\"\nkind = \"shell\"\nrun = \"true\"\n", 1).await;

    assert_eq!(out.status, RunStatus::Ok);
    assert!(!out.has(|k| matches!(k, EventKind::RunBranchCreated { .. })));
    assert!(
        !git::branch_exists(&h.repo, &run_branch_name(out.run_id)).await.unwrap(),
        "a shell-only run must behave exactly as it did in M1"
    );
}

#[tokio::test]
async fn a_missing_provider_binary_fails_the_node_with_a_useful_message() {
    let h = Harness::new().await;
    let src = "[providers.gone]\ncmd = \"definitely-not-real-xyz\"\nargs = [\"{prompt}\"]\n\
               [[task]]\nid = \"n\"\nkind = \"agent\"\nprovider = \"gone\"\nprompt = \"x\"\n";

    let out = h.run(src, 1).await;

    assert_eq!(out.status, RunStatus::Partial);
    assert!(
        out.has(|k| matches!(k, EventKind::NodeFailed { reason, .. } if reason.contains("definitely-not-real-xyz"))),
        "{:?}",
        out.events
    );
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test --test agent_nodes`
Expected: FAIL — `RunOpts` has no `repo` field, and agent nodes still report `AGENT_UNSUPPORTED`.

- [ ] **Step 4: Write the implementation**

In `src/scheduler.rs`:

- Extend `RunOpts` as specified above.
- At the top of `execute`, when `graph.tasks` contains an agent node **and** `opts.repo` is `Some`, create the run branch and integration worktree, append `RunBranchCreated`, and use the integration worktree as the working directory for every node. Otherwise keep `opts.cwd`.
- Hold `Arc<Mutex<PathBuf>>` for the integration worktree; take the lock only around merges.
- Replace the `TaskKind::Agent` arm with the real flow described above. Because a node now emits several events from inside a spawned task, have the task return a richer `NodeCompletion` describing what happened (`Committed { sha, stat }`, `NothingToCommit`, `Conflicted(Vec<String>)`, `Failed(String)`) and let the **loop** write the events, keeping all log writes on one task and preserving ordering.

Sketch of the completion type:

```rust
/// What a finished node reports back to the loop. Events are written by the
/// loop, not the task, so ordering in the log stays deterministic.
enum NodeResult {
    Succeeded,
    Committed { sha: String, stat: git::DiffStat, merged: Option<String> },
    Conflicted { paths: Vec<String> },
    Failed { reason: String },
}
```

In `src/main.rs`, populate the new `RunOpts` fields: `repo: Some(repo_root.clone())` and `seed_from: std::env::current_dir()?`. Record `run_branch` and `base_sha` in `RunMeta` once the branch exists — write meta again after the run branch is created, since the id is not known before.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --test agent_nodes`
Expected: PASS, 9 tests.

Then remove the obsolete M1 test `an_agent_node_fails_with_a_clear_not_yet_supported_reason` from `tests/scheduler.rs` and delete `AGENT_UNSUPPORTED`.

Run: `cargo test`
Expected: PASS across all files.

- [ ] **Step 6: Commit**

```bash
just check
JJ_EDITOR=true jj describe -m "feat(scheduler): run agent nodes in worktrees and merge into the run branch"
JJ_EDITOR=true jj new
```

---

### Task 7: Cleanup, gc, and end-to-end verification

**Files:**
- Modify: `src/cli.rs`, `src/main.rs`, `justfile`
- Test: `tests/cli.rs` (extend)

**Interfaces:**
- `cli::Command::Gc { #[arg(long)] older_than: Option<String>, #[arg(long)] dry_run: bool }`
- `main` gains `fn remove_stale_worktrees(...)`.

`gc` removes worktree directories under `~/.assembly/wt/` for runs whose state directory no longer exists or that are older than the given duration, then runs `git worktree prune`. Successful nodes already discard their own worktrees; `gc` collects what failures left behind.

- [ ] **Step 1: Write the failing test**

Append to `tests/cli.rs`:

```rust
#[test]
fn gc_dry_run_reports_without_deleting() {
    let tmp = repo();
    write_graph(&tmp, "[[task]]\nid=\"a\"\nkind=\"shell\"\nrun=\"true\"\n");
    assembly(&tmp).args(["run", "graph.toml"]).assert().success();

    assembly(&tmp)
        .args(["gc", "--dry-run"])
        .assert()
        .success();
}

/// A repo with a real commit, which a run branch requires.
fn repo_with_commit() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let at = tmp.path().to_str().unwrap();
    for args in [
        vec!["-C", at, "init", "-q", "--initial-branch=main"],
        vec!["-C", at, "config", "user.email", "t@e.com"],
        vec!["-C", at, "config", "user.name", "T"],
        vec!["-C", at, "config", "commit.gpgsign", "false"],
    ] {
        assert!(
            std::process::Command::new("git").args(&args).status().unwrap().success(),
            "git {args:?}"
        );
    }
    std::fs::write(tmp.path().join("README.md"), "base\n").unwrap();
    for args in [
        vec!["-C", at, "add", "-A"],
        vec!["-C", at, "commit", "-qm", "init"],
    ] {
        std::process::Command::new("git").args(&args).status().unwrap();
    }
    tmp
}

#[test]
fn meta_records_the_run_branch_only_when_the_graph_has_agent_nodes() {
    let tmp = repo_with_commit();
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/fake-agent.sh");

    std::fs::write(
        tmp.path().join("graph.toml"),
        format!(
            "[providers.fake]\ncmd = \"bash\"\nargs = [\"{}\", \"{{prompt}}\", \"x\"]\n\
             [[task]]\nid = \"n\"\nkind = \"agent\"\nprovider = \"fake\"\nprompt = \"hi\"\n",
            script.display()
        ),
    )
    .unwrap();

    assembly(&tmp).args(["run", "graph.toml"]).assert().success();
    let meta = std::fs::read_to_string(tmp.path().join(".assembly/runs/1/meta.json")).unwrap();
    assert!(meta.contains("al/run-1"), "{meta}");

    // A shell-only run creates no branch, so records none.
    std::fs::write(
        tmp.path().join("shell.toml"),
        "[[task]]\nid=\"a\"\nkind=\"shell\"\nrun=\"true\"\n",
    )
    .unwrap();
    assembly(&tmp).args(["run", "shell.toml"]).assert().success();
    let meta2 = std::fs::read_to_string(tmp.path().join(".assembly/runs/2/meta.json")).unwrap();
    assert!(!meta2.contains("al/run-2"), "{meta2}");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --test cli`
Expected: FAIL — `gc` is not a subcommand.

- [ ] **Step 3: Write the implementation**

Add the `Gc` variant to `cli::Command`, and a handler in `main.rs` that lists `~/.assembly/wt/*`, matches each against `<git-root>/.assembly/runs/<id>`, removes orphans (respecting `--dry-run`), and calls `git::prune_worktrees`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Verify end to end by hand**

```bash
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-/tmp/assembly-line-target}
just build
BIN=$CARGO_TARGET_DIR/debug/assembly
D=$(mktemp -d) && cd "$D" && git init -q . && git config user.email t@e.com && git config user.name T
echo base > README.md && git add -A && git commit -qm init
cat > graph.toml <<EOF
[providers.fake]
cmd = "bash"
args = ["/Users/harry-mac/code-stuff/assembly-line/tests/fixtures/fake-agent.sh", "{prompt}", "x"]

[[task]]
id = "write-docs"
kind = "agent"
provider = "fake"
prompt = "document the thing"

[[task]]
id = "check"
kind = "shell"
needs = ["write-docs"]
run = "test -f agent-output.txt && echo saw the merged work"
EOF
"$BIN" run graph.toml; echo "exit=$?"
"$BIN" status
git branch --list 'al/*'
git log --oneline al/run-1 | head -5
git status --porcelain   # must be empty: your tree was never touched
```

Expected: exit 0; `status` shows both nodes done with a diff stat on `write-docs`; `al/run-1` exists with the agent's commit merged; the working tree is clean and still on the original branch.

- [ ] **Step 6: Commit**

```bash
just check
JJ_EDITOR=true jj describe -m "feat(cli): gc for worktrees left behind by failed nodes"
JJ_EDITOR=true jj new
```

---

### Task 8: Update the spec and milestone docs

**Files:**
- Modify: `docs/superpowers/specs/2026-08-15-assembly-line.md`
- Create: `docs/superpowers/plans/2026-08-15-assembly-line-m2.md` progress notes at the bottom of this file

- [ ] **Step 1: Record what shipped and what did not**

In the spec:

- Mark M2 complete in the Milestones table.
- Under "Accepted risks", add: *a merge into the run branch can land while an unrelated shell node is running in the integration worktree; M3's approval queue serializes this.*
- Under "Accepted risks", add: *an agent node with no `verify` and no gate has nothing checking its output in M2; `validate` warns.*
- Confirm the branch-naming section matches the implementation.

- [ ] **Step 2: Final verification**

```bash
just check
just verify-stack
```

Expected: every change reports `ok`; no `FAILED`.

- [ ] **Step 3: Commit**

```bash
JJ_EDITOR=true jj describe -m "docs: record M2 scope, limitations, and accepted risks"
```

---

## M2 Definition of Done

- [ ] `just check` clean: tests, clippy pedantic, formatting.
- [ ] `just verify-stack` reports `ok` for every change.
- [ ] An agent node runs a provider command in its own worktree and its work reaches the run branch.
- [ ] A prompt containing quotes, `$`, and `;` reaches the agent as one argument, unexpanded.
- [ ] A dependent node sees its upstream's merged work.
- [ ] A failing agent fails the node and **keeps** its worktree; a successful one discards it.
- [ ] An agent that changes nothing succeeds without a commit or merge.
- [ ] Two agents touching the same file produce a recorded conflict and one failed node.
- [ ] Seeded `copy` files reach the agent and never reach a branch.
- [ ] A shell-only graph creates no branch and no worktrees — M1 behaviour is unchanged.
- [ ] After any run, `git status --porcelain` in the target repo is empty and HEAD is unmoved.
- [ ] No `AgentRunner` trait exists.
