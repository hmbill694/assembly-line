# assembly-line

A Rust CLI that runs one coding-agent job — a repo, a ref, and a prompt — as a
branch, decides whether it succeeded with `verify`, and opens a pull request
when it did.

- Design decisions: `docs/superpowers/specs/2026-09-11-software-factory-v2.md`
- Current milestone: `docs/superpowers/plans/2026-09-11-software-factory-f1.md`

## Toolchain

Edition 2024, pinned to stable 1.97.1 via `rust-toolchain.toml`. Do not
downgrade the edition to work around a compile error — fix the code.

```
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt
```

All three must be clean before a commit.

## Version control: a stack of always-compilable jj changes

This repo uses **jj (Jujutsu)**, colocated with git. Work is organized as a
stack of small changes, and **every change in the stack must build, test,
lint, and format cleanly on its own** — not just the tip. A stack that only
compiles at the end breaks `bisect` and makes per-change review meaningless.

One concern per change, each carrying the tests for what it adds. A test file
belongs to the change that introduces the module it *imports*, which is not
always the change it feels related to.

### Building a stack

Split bottom-up by path, then walk it to fix up the declaration files:

```
JJ_EDITOR=true jj split -m "feat(x): ..." src/x.rs tests/x.rs
```

`lib.rs` grows one `pub mod` line per change. Path-based splitting puts the
whole final file in one change, so afterwards `jj edit <rev>` each revision
and write the module list correct for that point — jj auto-rebases
descendants. When the manifest declares a binary, the scaffold change needs a
stub `main.rs`; the real one lands with the CLI change.

### Verifying the stack

`just verify-stack`. It exports each revision with `git archive` into a temp
directory and builds it there, which matters for two reasons learned the hard
way:

- **Never `jj edit` your way down the stack to verify.** As soon as a bookmark
  points into the stack, those changes are immutable and `jj edit` fails — and
  under `set -e` the loop dies silently with no output. Exporting touches
  nothing.
- **Never share one `CARGO_TARGET_DIR` across revisions without
  `cargo clean -p <crate>` between them.** Cargo will reuse the previous
  revision's artifacts for the same package and report a *correct* revision as
  broken — an unresolved-import error for a module the revision plainly
  declares. Cleaning only our package keeps the expensive dependency
  artifacts. It cleans on the way *out* too: the last revision's test binaries
  have `env!("CARGO_MANIFEST_DIR")` baked in pointing at the temp tree
  `verify-stack` just deleted, and a later `just test` that reused them fails
  on every fixture path with a mystifying "no such file".

Build output must also live outside the repo: revisions below the one that
adds `.gitignore` will otherwise snapshot `target/` into the change. `target/`,
`.assembly/`, and `.devenv/` are in `.git/info/exclude` for the same reason.

Test counts should climb monotonically up the stack.

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
for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
    if let Some(id) = entry.file_name().to_str().and_then(|s| s.parse::<u64>().ok()) {
        out.push((id, entry.path()));
    }
}

// yes
std::fs::read_dir(dir)
    .into_iter()
    .flatten()
    .flatten()
    .filter_map(|entry| {
        let id = entry.file_name().to_str()?.parse::<u64>().ok()?;
        Some((id, entry.path()))
    })
    .collect()
```

(`src/gc.rs`'s `job_directories`.)

### Prefer pattern matching to if-else chains

Match on the *shape* of the data — tuples of the relevant fields, `Option`
combinations, enum variants. Match guards are good; nested `if` inside a match
arm usually means the tuple should have been wider.

```rust
// no
if let Some(text) = prompt {
    Ok(text)
} else if let Some(path) = prompt_file {
    std::fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))
} else {
    Err("a job needs --prompt or --prompt-file".into())
}

// yes
match (prompt, prompt_file) {
    (Some(text), _) => Ok(text),
    (None, Some(path)) => {
        std::fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))
    }
    (None, None) => Err("a job needs --prompt or --prompt-file".into()),
}
```

(`src/main.rs`'s `prompt_text`.)

### Build values, don't mutate them

Prefer `let x = match ... ` and `let x: Vec<_> = ....collect()` over declaring
a mutable binding and filling it in. `let ... else` for early exits;
`bool::then` / `then_some` to turn a condition into an `Option`.

### Functions return values, not side effects

A function that both computes and writes is two functions. Keep the pure part
separately testable — this is why `JobReport::from_events` folds a job's
event stream into a report with no I/O of its own, and turning that report
into text (`to_summary_line`, `to_duration_line`) is a separate step
(`src/report.rs`).

### Where loops are still correct

Don't contort this into an iterator chain: sequential I/O with early return on
error, where `?` inside a loop reads better than `collect::<Result<_, _>>()`
would. `git::commit_all_except` unstages each `never_commit` path this way
(`src/git.rs`) — a genuine loop, not a map in disguise.

When you do write one, it should be because the alternative is worse, and
that should be obvious to the next reader. F1 runs exactly one job at a time,
so there is no scheduler loop any more to hold up as the headline case — if a
later milestone's daemon brings one back, it belongs here.

### Cost

The unit of work is one job, and the collections around it — a repository's
declared providers, its `copy` list, the remotes `git remote` prints, a `gc`
run's stale worktrees — are a handful of items, not millions. Favor clarity:
`git::remote_exists` checks membership by scanning `git remote`'s output line
by line rather than collecting it into a `HashSet` first (`src/git.rs`); with
a handful of remotes the scan is clearer and the difference in cost does not
exist. There is no graph left to traverse — no DFS, no Kahn's-style peeling;
a job either runs or it doesn't. If a later milestone's daemon runs many jobs
at once, the cost question becomes scheduling contention, not walking a data
structure — revisit this section when that lands.

## Naming

A name should tell the reader what the thing *is* or *decides*, without them
opening it. Bare verbs (`check`, `handle`, `process`, `absorb`, `skip`) and
bare nouns (`data`, `info`, `result`, `entry`) fail that test.

- **Predicates read as claims:** `git::branch_exists`, not `exists`.
- **Filters name what they select:** `gc::collectable`, not `filtered` — it
  names the `RepositoryLeftovers` a `gc` run would actually remove
  (`src/gc.rs`).
- **Error producers name the fault:** `RepoConfig::reasons_it_cannot_run`,
  `unparseable_max_duration` — not `problems`, `duration_error`
  (`src/config.rs`).
- **Mutators name the transition, including its scope:** `EventLog::append`,
  not `write` — the log is append-only, so the half of the name that rules out
  rewriting is the half that earns its place (`src/event.rs`).
- **Fields carry their unit or role:** `attempt_started_at`,
  `last_attempt_duration`, `committed_diff` — not `started`, `duration`,
  `diff` (`JobProgress` in `src/report.rs`).
- **Constructors say where the value came from:** `JobReport::from_events`,
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
  users want every problem in their `.assembly/config.toml` at once, not one
  per run.
- Every error type implements `Display` with a message that says what to do
  about it, not just what went wrong.

## Testing

- Integration tests in `tests/`, one file per module concern.
- Test through the public API. `JobState::replay` and `JobReport::from_events`
  are both pure folds over an event stream, so most behavior can be asserted
  by feeding them events, with no processes involved (`src/state.rs`,
  `src/report.rs`).
- Agent execution is tested with **shell-script fakes**, never a real API. No
  test may touch the network or require credentials.
- Prove concurrency with observable evidence — a wall-clock bound, or a probe
  that records how many copies of a command were live at once — not by
  inspecting internal state. This is forward-looking, not descriptive of F1's
  own tests: F1 runs exactly one job at a time, so there is nothing concurrent
  to observe today, and the DAG scheduler's wall-clock probes were deleted
  with it. Apply this rule when a later milestone's daemon actually runs jobs
  concurrently — don't go looking for the tests it describes before then.

## Invariants

- The event log is **append-only**. Never rewrite or truncate it.
- `JobState::apply` must stay a pure function of the event stream. Anything
  that cannot be reconstructed from `events.jsonl` does not belong in
  `JobState` or `JobReport`.
- Job ids are never user input, so there is nothing to validate. A job's id is
  a `u64` that `paths::next_job_id` allocates by scanning the existing job
  directories and taking one past the max, and its branch name is derived
  from that id alone (`al/job-{id}`, `workspace::job_branch_name`) — always a
  well-formed git ref, with no pattern check needed because nothing
  user-authored ever reaches it.
- The target repository is never modified beyond `.assembly/jobs/`, where a
  job's event log and metadata live until a later milestone moves that state
  out of the repository entirely (`src/paths.rs`). Worktrees live under
  `$HOME` (or `$ASSEMBLY_WORKTREE_ROOT`, which tests set), never inside the
  repo: the target repo must stay untouched, and a worktree inside it would
  need a `.gitignore` entry assembly-line is not entitled to add.
