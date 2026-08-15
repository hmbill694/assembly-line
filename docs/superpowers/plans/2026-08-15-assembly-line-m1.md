# assembly-line M1 — Task Checklist

**Spec:** `docs/superpowers/specs/2026-08-15-assembly-line.md`

**Goal:** `assembly` parses a TOML task graph, validates it, runs shell nodes in
parallel with dependency ordering, records every transition to an append-only
event log, resumes an interrupted run by replaying that log, and skips the
subtree below a failed node.

**Architecture:** library crate `assembly_line` + thin binary `assembly`.
`Dag::build` validates and indexes the graph. A tokio scheduler drives ready
nodes as `JoinSet` tasks under a job cap and a resource-exclusivity set. All
state changes go through `events.jsonl`; `RunState` is a pure fold over it,
which makes resume a replay and makes tests state assertions rather than
timing assertions.

## Constraints

- Rust **edition 2024**, toolchain pinned to **1.97.1** via `rust-toolchain.toml`.
- M1 executes `kind = "shell"` only. The config model parses the *full* schema
  (agent fields, providers, hooks, workspace) so later milestones add no
  parsing churn; `run` fails an agent node with `agent nodes are not supported
  yet (M2)`.
- Config enums are kebab-case (`on-complete`); every struct uses
  `#[serde(deny_unknown_fields)]` so a typo is an error, not a silent default.
- Task ids must match `^[A-Za-z0-9_-]+$` — they become filenames and branch names.
- Event log is append-only, one tagged JSON object per line.
- Run state at `<git-root>/.assembly/runs/<id>/`. (`~/.assembly` is M5.)
- Exit codes: `0` ok, `1` partial/aborted, `2` validation or usage error.
- No network anywhere in M1; every test passes offline.

---

## Tasks

Each task is TDD: write the tests, watch them fail, implement, watch them pass.

### 1. Scaffold + config model — `src/config.rs`
`Graph`, `Workspace`, `Provider`, `Hook`, `Task`, `TaskKind`, `Supervise`,
`OnFailure`, `parse_graph`, `load_graph`, `parse_duration`.

- [x] parses a full graph (both kinds, providers, hooks, workspace)
- [x] applies defaults (`supervise = none`, `on_failure = skip`, `retries = 0`)
- [x] rejects unknown fields
- [x] parses and rejects durations

### 2. DAG + validation — `src/dag.rs`, `src/cli.rs`
`Dag::{build, ids, needs, dependents, descendants}`, `ValidationError`,
`Warning`, `Validation`, `validate`. `assembly validate` command.

- [x] builds a diamond, reports descendants
- [x] rejects duplicate ids, unknown deps, cycles, self-deps
- [x] rejects ids unsafe as paths (`../etc`)
- [x] rejects shell-without-`run`, agent-without-`prompt`, unknown provider
- [x] rejects invalid `max_duration`
- [x] warns: unsupervised agent without `verify`; `max_cost_usd` without adapter

### 3. Run directory layout — `src/paths.rs`
`git_root`, `runs_root`, `next_run_id`, `latest_run_id`, `create_run`,
`open_run`, `RunPaths{events,meta,logs_dir,log}`, `RunMeta`, `write_meta`,
`read_meta`.

- [x] finds git root by walking up; `None` outside a repo
- [x] allocates monotonic run ids, ignoring non-numeric dirs
- [x] lays out the run directory; `open_run` fails for a missing run
- [x] `RunMeta` round-trips

### 4. Event log — `src/event.rs`
`Event`, `EventKind`, `RunStatus`, `EventLog::{open_append, append, read}`.

- [x] appends and reads back in order
- [x] reopening appends rather than truncating
- [x] each line is one tagged JSON object (`t`, `at`, payload)
- [x] reading a missing file is empty
- [x] a torn final line (crash mid-write) is skipped, not fatal

### 5. Run state — `src/state.rs`
`NodeState`, `RunState::{new, apply, replay, reset_running, ready, state,
counts}`, `TaskMap`, `task_map`.

Readiness: `Pending` and every dep `Done`, or `Failed` on a task with
`on_failure = continue`. `Skipped` never satisfies.

- [x] only roots ready initially; finishing unlocks both branches
- [x] a join waits for every dependency
- [x] a skipped dep never satisfies; a failed-with-continue dep does
- [x] replay reconstructs state; `reset_running` requeues in-flight nodes
- [x] counts summarize; `run_finished` records status

### 6. Shell execution — `src/exec.rs`
`ShellOutcome`, `run_shell(cmd, cwd, log_path, timeout, cancel)`.

Two append-mode fds on one log file — no reader tasks, output survives a kill.

- [x] captures stdout and stderr; reports exit codes
- [x] runs in the given working directory
- [x] timeout kills the child, `timed_out = true`
- [x] cancellation kills the child, `cancelled = true`
- [x] appends across runs rather than truncating

### 7. Scheduler — `src/scheduler.rs`
`RunOpts`, `execute`.

- [x] runs a linear chain in order
- [x] independent nodes actually overlap (wall-clock assertion)
- [x] `--jobs` cap is respected (observed concurrency probe)
- [x] nodes sharing a `resource` never overlap
- [x] writes a log file per node; emits started/finished events
- [x] an agent node fails naming M2

### 8. Failure semantics + `run` command — `src/main.rs`
- [x] failure skips exactly its subtree; independent branches finish
- [x] `on_failure = continue` lets dependents run
- [x] `on_failure = abort` cancels in-flight work and skips the rest
- [x] a timed-out node fails with that reason
- [x] CLI: exit 0 / 1 / 2; refuses to run outside a git repo

### 9. Resume — `src/main.rs`
- [x] `resume` does not re-run completed nodes
- [x] `resume` appends to the same log (one `run_started` per attempt)

### 10. `status` and `logs` — `src/report.rs`
`NodeReport`, `RunReport`, `summarize`, `render`.

- [x] summarizes states, durations, and failure/skip detail
- [x] a retried node reports its last attempt
- [x] renders every node plus a status line
- [x] an unfinished run has no status

---

## Definition of Done

- [x] `cargo test` passes, no ignored tests
- [x] `cargo clippy --all-targets -- -D warnings` clean
- [x] `cargo fmt --check` clean
- [x] edition 2024 on pinned toolchain 1.97.1
- [x] parallelism, job cap, and resource exclusivity demonstrated by tests
- [x] kill mid-run then `assembly resume <id>` does not repeat completed nodes
- [x] agent node fails with a reason naming M2 rather than doing something surprising
