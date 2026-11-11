# Arch 09 — `run` speaks of rounds

Finding #9 of `.architecture-reports/2026-10-17-architecture-review.html`,
grilled with the user on 2026-10-29. **Status:** built and reviewed.
Picked up from
`docs/superpowers/plans/2026-10-20-architecture-handoff.md`.

## Goal

The code, docs and help text around `assembly run` say "round" for the
things that belong to one round, and keep "job" for the durable thing, as
`CLAUDE.md`'s vocabulary says. After `run`, the summary line says that the
verdict is this round's. `payload::RoundRequest` gets a name that does not
echo `EventKind::RoundRequested`, so #7 can use `RoundRequest` for the
job's latest request.

## Why this shape

Evidence checked at `smnununx` and against #4's change (`lopouzot`). #4
reshaped `run.rs`, so the building agent checks every line reference
again on its base.

| Fact | Where |
|---|---|
| `run.rs`'s module doc says "one whole job, in this process"; `tests/run.rs:1` repeats it | `src/run.rs:1-2` |
| `run`'s help says "Do one whole job, here, in this process"; `about` says "Run one coding-agent job …" | `src/cli.rs:8`, `:22-23` |
| `run_whole_job`, whose doc says "The exit code mirrors the job" | `src/main.rs` |
| `ReadyJob` is an admitted round that holds one `RoundPayload` | `src/run.rs` |
| `JobConclusion { job, verdict, report, base_differs }`: `verdict` is one round's; `report` folds only this process's frames | `src/run.rs:107` (in #4) |
| `payload::RoundRequest` ("what `run` knows about a round before its settings have been applied") sits next to the unrelated `EventKind::RoundRequested` | `src/payload.rs:76`, `src/event.rs:15` |
| `for_round` sets `provision_toolchain: false`; `run.rs` overwrites it with struct-update syntax; `tests/payload.rs:44` pins the `false` | `src/payload.rs:100`, `src/run.rs` |
| `run` never writes `RoundStarted`, so its report has `rounds == 0` and the line prints `job 1: passed` for a revise | `tests/cli.rs:449` |
| Docs that say "job" for one round: `RunRefused`, `prepare_run`, `admission.rs` ("Whether a job may run"), `delivery.rs:1`, `tests/support/mod.rs` helper docs and panic messages | various |

## Success criteria

1. **Names.**
   - `run::ReadyJob` → `run::AdmittedRound`;
   - `run::JobConclusion` → `run::RoundConclusion`, and its field `report`
     → `this_rounds_report`;
   - `main.rs`'s `run_whole_job` → `run_one_round`;
   - `payload::RoundRequest` → `payload::RoundBeforeSettings`.
   - No item named `RoundRequest` is left in `src/` (#7 takes the name).
   - `RunRefused`, `RunRequest` and `prepare_run` keep their names: they
     name the `run` command.
2. **Text.** Docs and comments in `run.rs`, `main.rs`, `admission.rs`,
   `delivery.rs`, `payload.rs`, `tests/run.rs` and `tests/support/` say
   "round" where they mean one round. `run`'s help and `about` say that
   `run` does one round. A grep for "whole job" in `src/` and `tests/`
   finds nothing.
3. **The summary line after `run`** is `job N: round passed` /
   `job N: round failed`, with the same diff, facts and detail as today:
   for example `job 1: round failed (1 file +1/-0) — verify failed`.
   `RoundConclusion::to_lines` prints it. `JobReport::to_summary_line` and
   everything `status` and `logs` print do not change.
4. **`provision_toolchain`** is a field of `RoundBeforeSettings`, and
   `RoundPayload::for_round` sets it once. No struct-update overwrite is
   left in `run.rs`. `tests/payload.rs` asserts that the flag passes
   through.
5. `just check` is clean and the test count does not fall (it should rise
   by the summary-line test). `just verify-stack` passes after the merge
   (the supervisor runs it).

## Stated behaviour change

- The last summary line of `assembly run` says `round passed` /
  `round failed` instead of `passed` / `failed`. The tests that pin the old
  text after `run` change: `tests/cli.rs:106`, `:124`, `:159`, `:450`,
  `tests/run.rs:85`, `:190`, and any the agent finds. The daemon's
  `status` lines (`tests/daemon_jobs.rs`, `tests/report.rs`) do not
  change.
- `run`'s `--help` text changes.

## In scope

- `src/run.rs`, `src/main.rs`, `src/cli.rs`, `src/payload.rs`,
  `src/admission.rs` (docs only), `src/delivery.rs` (docs only).
- `tests/run.rs`, `tests/cli.rs`, `tests/payload.rs`, `tests/support/`, and
  any test that names a renamed item.
- A test that pins the new summary line after a revise (`run --job 1`).

## Out of scope

- `RoundPayload.remote_url` (dead): #6 deletes it.
- `JobSecrets`, `JobQueue`, and every name in `src/daemon/*` and
  `src/runner/*`.
- The announcement lines ("job {}: claimed …", "job {}: continuing …") and
  the messages that are really about the job ("run without --job to start
  a new job", "job {id} has no branch …", "a job needs --prompt or
  --prompt-file").
- `JobReport`, the fold, and `status` / `logs` output.
- #7's work on `JobReport`'s request fields (it builds at the same time,
  in `report.rs`, `daemon/*` and `runner/mod.rs`).

## Constraints

- Everything in `CLAUDE.md`, including one jj change that builds, tests,
  lints and formats on its own; `main.rs` holds no logic and no message
  text (the summary line's text lives in the library).
- Names follow `CLAUDE.md`'s Naming section.

## Decisions

| # | Decision | By |
|---|---|---|
| 1 | Rename code, fix docs and comments, and fix `run`'s help and `about` | user (Q1) |
| 2 | The summary line after `run` is `job N: round passed` / `round failed`, with no round number | user (Q2) |
| 3 | Names: `AdmittedRound`, `RoundConclusion`, `run_one_round`, `payload::RoundBeforeSettings`; `RunRefused`, `RunRequest`, `prepare_run` stay | user (Q3) |
| 4 | `RoundConclusion.report` → `this_rounds_report: JobReport`; `RoundConclusion::to_lines` prints the round's summary; `JobReport::to_summary_line` unchanged | user (Q4) |
| 5 | `provision_toolchain` folds into `RoundBeforeSettings`; `remote_url` is left for #6 | user (Q5) |
| 6 | `JobSecrets` and `JobQueue` keep their names | user (Q6) |
| 7 | How `to_lines` builds the round's summary from the report (a new `JobReport` method or a local format), and the exact help wording | delegated |
| 8 | Review findings 1 and 2: `src/report.rs` is in scope for them. `report.rs` exposes the shared pieces (the facts after the state label, and the `branch:` / `pull request:` lines); `RoundConclusion::to_lines` calls them instead of copying their format; `JobReport::to_conclusion_lines` is deleted and its test (`tests/report.rs`) moves to `RoundConclusion::to_lines`. Wave 2's merge order is now #7 then #9, so #9 builds on #7's `report.rs` | user (mid-build) |
| 9 | Fix two doc leftovers: the example in `JobReport::to_summary_line`'s doc (`src/report.rs`) uses text `status` really prints; the design spec (`docs/superpowers/specs/2026-09-11-software-factory-v2.md`, about lines 285 and 607) says "round" where it means one round | user (mid-build) |

## Assumptions

- Nothing outside the tests parses `run`'s summary line (a search of
  `src/`, `tests/`, `scripts/`, `justfile` and `README.md` found only
  tests). The daemon's collector reads frames, not this line.

## Open questions

None.

## Implementation notes

Order of work: tests first, then code.

1. **Tests.** Change the tests that pin `job N: passed` after `run` to the
   new text, and add the revise test; they fail first. Change
   `tests/payload.rs:44` for the folded flag.
2. **Code.** The renames (the compiler finds every site), the summary line,
   the payload flag, then the docs, comments and help.
3. **Docs.** Update this brief's status line. Do not edit the handoff: the
   supervisor updates it after the merge. Apply the de-ai standard to the
   added lines.
4. **Checks.** `just check` in the workspace. Record the test count in the
   commit body.

## jj

#9 is in wave 2 of the handoff's "Waves", on the merged wave 1. Build in
the working-copy change of the jj workspace the supervisor made for it.
Describe it as:

```
refactor(run): run speaks of rounds

<why, the stated behaviour change, and: Tests: N (was M).>
```
