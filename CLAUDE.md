# assembly-line

A Rust CLI that executes a DAG of tasks — shell commands and coding-agent
sessions — in parallel, supervised or unsupervised.

- Design decisions: `docs/superpowers/specs/2026-08-15-assembly-line.md`
- Current milestone: `docs/superpowers/plans/2026-08-15-assembly-line-m1.md`

## Toolchain

Edition 2024, pinned to stable 1.97.1 via `rust-toolchain.toml`. Do not
downgrade the edition to work around a compile error — fix the code.

```
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt
```

All three must be clean before a commit.

## Style: functional by default

Write declarative Rust. Describe *what* the result is, not the steps to
accumulate it.

### Prefer iterators to loops

A `for` loop that pushes into a `Vec` is almost always a `map`, `filter_map`,
`flat_map`, or `collect`. Reach for `fold` when you genuinely need to thread
state, and `std::iter::successors` for "keep going until" sequences.

```rust
// no
let mut out = Vec::new();
for t in tasks {
    if t.kind == TaskKind::Agent {
        out.push(t.id.clone());
    }
}

// yes
let out: Vec<String> = tasks
    .iter()
    .filter(|t| t.kind == TaskKind::Agent)
    .map(|t| t.id.clone())
    .collect();
```

### Prefer pattern matching to if-else chains

Match on the *shape* of the data — tuples of the relevant fields, `Option`
combinations, enum variants. Match guards are good; nested `if` inside a match
arm usually means the tuple should have been wider.

```rust
// no
if t.kind == TaskKind::Shell && t.run.is_none() {
    Some(ShellMissingRun(t.id.clone()))
} else if t.kind == TaskKind::Agent && t.prompt.is_none() {
    Some(AgentMissingPrompt(t.id.clone()))
} else {
    None
}

// yes
match (t.kind, &t.run, &t.prompt) {
    (TaskKind::Shell, None, _) => Some(ShellMissingRun(t.id.clone())),
    (TaskKind::Agent, _, None) => Some(AgentMissingPrompt(t.id.clone())),
    _ => None,
}
```

### Build values, don't mutate them

Prefer `let x = match ... ` and `let x: Vec<_> = ....collect()` over declaring
a mutable binding and filling it in. `let ... else` for early exits;
`bool::then` / `then_some` to turn a condition into an `Option`.

### Functions return values, not side effects

A function that both computes and writes is two functions. Keep the pure part
separately testable — this is why `RunState` is a fold over events and why
`report::summarize` is separate from `report::render`.

### Where loops are still correct

Don't contort these into iterator chains:

- **The scheduler's main loop.** It awaits completions, appends to the event
  log, and re-derives readiness. It is genuinely a state machine over time.
- **Sequential I/O with early return on error**, where `?` inside a loop reads
  better than `collect::<Result<_, _>>()` would.

When you do write a loop, it should be because the alternative is worse, and
that should be obvious to the next reader.

### Cost

Favor clarity; these graphs have tens of nodes, not millions. An O(n²)
`contains` over a small `Vec` is fine and clearer than threading a `HashSet`.
Where a naive functional formulation would be *asymptotically* bad — recursive
DFS on a diamond graph — restructure the algorithm (Kahn's-style peeling)
rather than reaching for mutable marks.

## Naming

A name should tell the reader what the thing *is* or *decides*, without them
opening it. Bare verbs (`check`, `handle`, `process`, `absorb`, `skip`) and
bare nouns (`data`, `info`, `result`, `entry`) fail that test.

- **Predicates read as claims:** `dependency_is_satisfied`, not `satisfied`.
- **Filters name what they select:** `nodes_clear_to_launch`, not `selectable`.
- **Error producers name the fault:** `unresolvable_dependencies`,
  `unparseable_max_duration` — not `edge_errors`, `duration_error`.
- **Mutators name the transition, including its scope:**
  `mark_pending_as_skipped`, not `skip` — the qualifier is the part that stops
  a reader assuming it skips everything.
- **Fields carry their unit or role:** `attempt_started_at`,
  `last_attempt_duration`, `resources_in_use` — not `started`, `duration`,
  `busy`.
- **Constructors say where the value came from:** `RunReport::from_events`,
  not `summarize`.

Longer is fine. The name is read far more often than it is typed.

## Abstraction: generic inputs, not speculative traits

**Program to an interface by accepting the most general type that works** —
`impl BufRead`, `AsRef<Path>`, `IntoIterator<Item = T>`, `impl Write`. Std's
traits already exist, everyone knows them, and they cost nothing after
monomorphization.

**Do not define a trait until a second implementor exists.** A single-impl
trait is indirection with no compile-time benefit: the reader has to chase the
impl to find what runs, and it pushes you toward `dyn` and `async_trait`
friction for nothing. This is the opposite of the DI convention in Java/C#, and
it is deliberate.

A trait earns its place at a real substitution seam — somewhere two
implementations exist *today*, typically because behavior differs between
running for real and running under test. Everywhere else, name the concrete
type.

Where a type owns a sink or source, make it generic with a sensible default
(`EventLog<W: Write = File>`) rather than inventing a trait around it.

## Error handling

- `anyhow` at the binary and I/O boundary.
- Validation returns a **`Vec` of typed errors**, not the first failure —
  users want every problem in their graph file at once, not one per run.
- Every error type implements `Display` with a message that says what to do
  about it, not just what went wrong.

## Testing

- Integration tests in `tests/`, one file per module concern.
- Test through the public API. `RunState` being a pure fold means most
  behavior can be asserted by feeding it events, with no processes involved.
- Agent execution is tested with **shell-script fakes**, never a real API. No
  test may touch the network or require credentials.
- Prove concurrency with observable evidence — a wall-clock bound, or a probe
  that records how many copies of a command were live at once — not by
  inspecting internal state.

## Invariants

- The event log is **append-only**. Never rewrite or truncate it.
- `RunState::apply` must stay a pure function of the event stream. Anything
  that cannot be reconstructed from `events.jsonl` does not belong in it.
- Task ids match `^[A-Za-z0-9_-]+$` — they become filenames and git branch
  names. Validate, never sanitize.
- The target repository is never modified. Run state lives under `.assembly/`;
  worktrees live outside the repo entirely.
