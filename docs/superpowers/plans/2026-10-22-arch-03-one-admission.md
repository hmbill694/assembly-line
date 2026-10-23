# Arch 03 — one admission, and a refusal is a verdict

Finding #3 of `.architecture-reports/2026-10-17-architecture-review.html`,
grilled with the user on 2026-10-20 to 2026-10-22. **Status:** built and
reviewed. Picked up from
`docs/superpowers/plans/2026-10-20-architecture-handoff.md`.

## Goal

One function decides whether a job may run. It produces the settings the
job runs under, and nothing else checks them again. The claim requires that
value. When `run --frames` refuses a round, the refusal reaches the job's
log as the round's verdict and its reason, instead of "exit 2".

## Why this shape

| Fact | Where |
|---|---|
| `run` and `submit` each read the config at the pinned base, resolve the provider, call `reasons_it_cannot_run`, then claim | `src/run.rs:160-196`, `src/daemon/submit.rs:85-136` |
| `for_round` looks up the provider and parses `max_duration` again | `src/payload.rs:94-121` |
| `command_limit_secs` parses `max_duration` a third time and drops a bad value without a message | `src/config.rs:160`, `src/daemon/dispatch.rs:433` |
| "No provider" travels as `""` | `run.rs:169`, `submit.rs:92`, `config.rs:134` |
| `run` refuses on stderr before any frame exists, so the collector writes "ended without reporting a verdict: exit 2" | `src/main.rs:113-136`, `src/frame.rs:292` |
| The two callers pin differently: `run` in its scratch clone, `submit` in the daemon's bare cache | `run.rs:159`, `submit.rs:82` |

## Success criteria

1. **One admission.** `admission::admit` takes a pinned base and the
   provider named on the command line (if any). It reads the config at the
   base's SHA and returns `Admitted { base, settings }`, or an
   `AdmissionRefused` that holds every reason. `prepare_run` and
   `submit::accept` both call it, and neither calls `RepoConfig::from_ref`
   or the validation itself.
2. **The claim requires admission.** `claim_job` takes `&Admitted` and
   claims at `admitted.base.sha`.
3. **No re-checks.**
   - `RoundPayload::for_round` takes `&JobSettings` and cannot fail.
   - `RepoConfig::command_limit_secs` is deleted.
   - The dispatcher gets the launch limit from `settings_for` on the config
     it reads from its cache.
4. **No `""` sentinel.** No named provider and no config default gives
   `ConfigError::NoProviderDeclared` directly.
5. **A refusal is a verdict.** Under `--frames`, any `RunRefused` writes
   exactly one `RoundFailed { reason }` frame. `run` still prints its
   `error:` lines on stderr and still exits 2.
   - `a_round_that_run_refuses_fails_with_the_reason_in_its_log`
     (`tests/daemon_jobs.rs:835`) asserts the real reason in `detail`:
     "job 2 has no branch on 'origin' — it was deleted, or the job never
     pushed one", without the `run`-only advice. `assembly logs 2` still
     shows the full stderr text.
   - Remove the matching "Known open" bullet from
     `docs/superpowers/plans/2026-09-28-software-factory-f3.md`.
6. **A hand-typed `run` without frames does not change:** the same `error:`
   lines and exit 2.
7. `just check` is clean, the test count rises from 370, and the event
   schema does not change.

## Stated behaviour changes

- A refused daemon round's `status` detail is the refusal's reason, not
  "the round ended without reporting a verdict: exit 2". This includes a
  round cancelled before its claim, which now reads "cancelled".
- A hand-typed `run --frames` that is refused prints one `RoundFailed`
  frame to stdout before it exits.
- An unparseable `max_duration` in a config the dispatcher reads at launch
  fails the round with the reason, instead of running it with no limit.
  This can only happen when the daemon is upgraded between submit and
  launch to a version with stricter rules.

## In scope

- New module `src/admission.rs`: `admit`, `Admitted`, `AdmissionRefused`
  (`Unreadable(anyhow::Error)`, `NotRunnable(Vec<ConfigError>)`). The text
  "`.assembly/config.toml` is not runnable" lives here only.
- `src/config.rs`:
  - `RepoConfig::settings_for(provider_named: Option<&str>) ->
    Result<JobSettings, Vec<ConfigError>>` replaces `reasons_it_cannot_run`
    and collects every error.
  - `JobSettings`: the provider's name and its `Provider`, `verify`, the
    pull request target `base`, `max_duration: Option<Duration>`, and
    `delivery`.
  - `settings_worth_flagging` moves onto `JobSettings`.
  - `command_limit_secs` is deleted.
- `src/claim.rs`: `claim_job(repo, remote, &Admitted)`.
- `src/payload.rs`: `for_round(&JobSettings, RoundRequest)`. `RoundRequest`
  loses `provider`, and the result is no longer a `Result`.
- `src/run.rs`:
  - `prepare_run` pins, admits, then claims.
  - `ReadyJob` holds `JobSettings` instead of `RepoConfig`, and `deliver`
    reads `base` and `delivery` from it.
  - `RunRefused` wraps `AdmissionRefused` instead of `ConfigNotRunnable`.
  - `RunRefused::to_failure_reason()` gives the frame's reason: the fault,
    without advice meant for a hand-typed `run`.
- `src/daemon/submit.rs`: pins, admits, claims with `&Admitted`, and reads
  warnings from `admitted.settings`.
- `src/daemon/dispatch.rs`: `settings_for(Some(&provider))` on the cached
  config. A refusal fails the round on the existing unreadable-config path,
  with every reason listed.
- `src/main.rs`: under `--frames`, write the refusal's frame before
  returning. The decision and the text stay in the library.
- Tests, and the F3 doc bullet.

## Out of scope

- Merging the two pins, or "checkout or URL?" (#6).
- The rest of the round lifecycle (#4); the latest request's four
  `Option`s (#7); `RUNS_IN_A_CONTAINER`, so the container checks in submit
  stay (#8); renames, except for the new types (#9); stringly SHAs and
  refs (#10).
- The event schema: `RoundRequested.provider` stays a `String`.
- `LaunchSpec.command_limit_secs` and `RoundPayload.command_limit_secs`
  stay `Option<u64>`, because they travel to the runners. Typing them is
  #10's ground.
- The collector, the fold and the runners. If any of them needs to change,
  the design is wrong: stop and ask the user.

## Constraints

- Everything in `CLAUDE.md`, including:
  - one jj change that builds, tests, lints and formats on its own;
  - no `#[cfg(test)]` in `src/`;
  - shell-script fakes only;
  - validation returns every error.
- `main.rs` only does process work. The refusal's frame text is decided in
  the library.
- `JobReport::from_events` does not change. Old logs still fold.

## Decisions

| # | Decision | By |
|---|---|---|
| 1 | Address both the single admission and the refusal in frames | user (Q1) |
| 2 | Done = criteria 1–7 | user (Q2) |
| 3 | Scope as above | user (Q3) |
| 4 | Two layers: pure `settings_for` → `JobSettings`, and I/O `admit` → `Admitted`, which `claim_job` requires. `tests/claim.rs` pays for this by committing a config and admitting before it claims | user (Q4) |
| 5 | The dispatcher re-runs `settings_for` on the cached config for the launch limit. A refusal there fails the round loudly | user (Q5) |
| 6 | Under `--frames`, every refusal writes one `RoundFailed` frame. No `RoundStarted` frame; stderr and exit 2 stay | user (Q6) |
| 7 | Each caller keeps its own pin. `admit` takes the `PinnedRef`, and `AdmissionRefused` owns the "not runnable" text | user (Q7) |
| 8 | The frame's reason states the fault without `run`-only advice. Stderr keeps the full text | user (Q8) |
| 9 | `admit` and its types live in a new `src/admission.rs`; `JobSettings` and `settings_for` live in `config.rs` | delegated |
| 10 | `for_round` cannot fail, so `tests/payload.rs`'s unknown-provider test moves to `tests/config.rs` as a `settings_for` test | delegated |

## Assumptions

- `JobSettings` can be built only through `settings_for`: its fields are
  public, and `#[non_exhaustive]` stops construction outside the crate. If
  a test outside the crate needs one, it goes through `RepoConfig::parse`
  and `settings_for`.
- `Admitted` can be built only through `admit`, and its fields are private
  (`base()`, `settings()`, `into_base_and_settings()`), so nothing can move
  its base after admission. The review found that public fields let a test
  claim a job at a commit that was never admitted.

## Open questions

None.

## Implementation notes

Order of work: tests first, then code.

1. **Tests.**
   - Flip `a_round_that_run_refuses_fails_with_the_reason_in_its_log`
     (criterion 5), and add a `run --frames` refusal test that parses
     stdout and finds exactly one `RoundFailed`.
   - In `tests/config.rs`, port the `reasons_it_cannot_run` tests (`:63`,
     `:73`, `:85`, `:96`, `:115`, `:150`, `:164`) to `settings_for`. Port
     `:103` to `max_duration`. Add one test for the resolved provider and
     one for the default provider.
   - `tests/claim.rs`: `published()` commits a runnable config and returns
     an `Admitted`. `tests/support/mod.rs:250` admits before it claims.
   - `tests/payload.rs`: build the settings through `settings_for`.
2. **Code**, in the order given in "In scope".
3. **Docs.** Update the F3 bullet and the handoff tracker. Run the de-ai
   standard over the added lines.
4. **Checks.** `just check`, then `just verify-stack`. Record the test
   count in the commit body.

## jj

Build in change `smnununx`, which sits on `nnzttxxs` (arch-02) and below
`prwrkokz`. Describe it as:

```
jj describe -m "refactor(admission): one admission, and a refusal is a verdict

<why, the stated behaviour changes, and: Tests: N (was 370).>"
```
