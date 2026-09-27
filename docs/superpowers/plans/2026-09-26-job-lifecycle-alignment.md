# The job lifecycle leaves `main.rs` — alignment brief

**Status:** agreed 2026-09-26, from finding 1 of the architecture review
(the job lifecycle has no home outside the binary). Findings 2–9 follow
in their own passes and reshape the code this one moves.

## Goal

`main.rs` handles only process concerns. Every job decision and every domain
message lives in the library, where tests call it directly. The test harness
stops copying the lifecycle. This is for locality and testability now; it is
not shaped for F3's daemon.

## Success criteria

1. `main.rs` meets the rule below.
2. `attempt_round`, `payload_from` and `round_from` are deleted from
   `tests/support/mod.rs`, and every test that runs a job goes through the
   library lifecycle with `LocalRunner::using(env!("CARGO_BIN_EXE_assembly"))`.
3. The assertions in `tests/cli.rs` pass unedited through step 7 of the stack:
   the same output text, exit codes and `.assembly/jobs/` layout.
4. Every change in the jj stack builds, tests, lints and formats on its own
   (`just verify-stack`), and the test count never drops between changes.

### The rule for `main.rs`

`main.rs` may:

- parse arguments
- read env
- handle signals
- run the tokio runtime
- choose stdout or stderr
- map exit codes
- build the concrete runner

Its `match`es only route a command to a call, or a variant to a stream or an
exit code. It makes no domain decisions, applies no domain defaults, and
writes the text of no domain message.

## In scope

- `run` and `revise`: they move into a library module (working name
  `lifecycle`).
- `status`: the library finds the job (the latest by default), and
  `JobReport` renders its lines.
- `logs`: the library resolves the log path, or refuses with "job N has
  captured no output yet". `main` prints the file or runs `tail -f`.
- `job-exec`: `JobPayload::from_environment()` reads and parses
  `ASSEMBLY_JOB`.
- Runner flag cross-checks: these move onto `RunnerArgs` in `cli.rs`, with
  today's messages.
- CLI tests put a fake `gh` first on the child's `PATH`.

## Out of scope

Each of these moves with its behaviour unchanged, and a later finding
reshapes it:

- The `RUNS_IN_A_CONTAINER` branches and where they live (finding 4).
- `rounds_so_far` and the four event folds (finding 3).
- Config being validated twice: in preflight, and again in
  `JobPayload::for_round` (finding 5).
- Bare `u64` job ids, and job naming living in `workspace.rs` (finding 7).
- The final name of the module and of `src/job.rs` (finding 2).
- Revise re-pinning the base at the remote's current tip (loose end 9).

## Constraints

- Logic moves as-is. The only reshaping allowed is what returning values
  instead of printing requires.
- An output change is allowed only as its own jj change, with its own
  `tests/cli.rs` edit. It is never mixed into a change that moves code.
- Everything in `CLAUDE.md` applies: functional style, the naming rules, a
  `Vec` of typed errors from validation, no trait without a second
  implementor, integration tests in `tests/`, no network access.

## Decisions

| # | Decision | By |
|---|---|---|
| 1 | Purpose: locality and testability now, not shaped for the daemon. | user |
| 2 | Done means the four success criteria above. | user |
| 3 | Finding 1 moves the code as-is. Later findings reshape it. | user |
| 4 | Two phases. `prepare` allocates nothing and returns notes or refusals. `run` allocates, runs the round and delivers. | user |
| 5 | `prepare_start` and `prepare_revision` produce one `ReadyRound`. It records its destination: a new job, or an existing job with its paths and next round. A single `run` does everything after that. | user |
| 6 | Delivery stays inside `run`. The harness's default config declares `[delivery] mode = "none"`. End-to-end delivery is tested in `tests/cli.rs` with a fake `gh` on the child's `PATH`. | user |
| 7 | `Refusal` is `ConfigNotRunnable(Vec<ConfigError>)`, `RunnerCannotRun(Vec<RunnerProblem>)` or `Unpreparable(anyhow::Error)`. Config is checked before the runner, as today. | user |
| 8 | Criterion 2 covers only the harness methods that run a job. `payload_for` stays as a runner-test fixture, built directly with `JobPayload::for_round`. | user |
| 9 | Adopt the rule for `main.rs`, and widen the scope to `status`, `logs`, `job-exec` and the runner flag checks. | user |
| 10 | Every message is `Display` on the value that decides it, including "revising job N (round R)" on `ReadyRound`. `main` only picks the stream. | user |
| 11 | The Ctrl-C `CancellationToken` is created in `main` (a process concern) and passed to `run`. | user |
| 12 | Working module name: `lifecycle`. | delegated |

## The stack

| # | Change | Output changes? |
|---|---|---|
| 1 | `test(cli)`: CLI tests put a fake `gh` first on the child's `PATH`, which closes a network leak — a passing job's default `pr` delivery ran whatever real `gh` the developer had | no |
| 2 | `refactor(cli)`: `RunnerArgs` names the flags that do not apply to the chosen runner, with today's messages | no |
| 3 | `refactor(payload)`: `JobPayload::from_environment()` for `job-exec` | no |
| 4 | `refactor(lifecycle)`: `prepare_start`, `prepare_revision`, `run`, `Refusal`, `ReadyRound`, and the conclusion value, each with its `Display`; `main` prints. New `tests/lifecycle.rs` for refusals and notes, with no round run | no |
| 5 | `test`: the harness runs jobs through the lifecycle and defaults to `delivery.mode = "none"`. `attempt_round`, `payload_from` and `round_from` are deleted. `revise_job` takes no round number. The refusal tests match on `Refusal`. `payload_for` becomes a plain fixture | no |
| 6 | `refactor(lifecycle)`: `status` finds its job in the library, and `JobReport` renders its lines | no |
| 7 | `refactor(lifecycle)`: `logs` resolves the log path, or refuses, in the library | no |
| 8+ | Optional output cleanups, one per change, each with its own `tests/cli.rs` edit | yes |

## Assumptions

- The per-test cost of spawning `assembly job-exec` is small next to the git
  clone that each job test already does.
- No test relies on the lighter collector's gap, where a round that ends
  without a verdict records none. If one does, it surfaces in step 5, and it
  is a behaviour question for the user rather than something to paper over.

## Open questions

None.
