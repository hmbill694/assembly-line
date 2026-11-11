# Arch 08 — a runner states what it requires, not one bit

Finding #8 of `.architecture-reports/2026-10-17-architecture-review.html`,
grilled with the user on 2026-10-29. **Status:** built and reviewed.
Picked up from
`docs/superpowers/plans/2026-10-20-architecture-handoff.md`.

## Goal

A runner states, as named facts, what the place it runs rounds needs:
which remotes it can reach, whether credentials are passed in, and whether
the toolchain must be provisioned. Each decision that today reads
`RUNS_IN_A_CONTAINER` reads the one fact it is about. The bit is gone.

## Why this shape

Evidence checked at `smnununx`. Waves 1–3 moved code: #4 moved
`credential_helper_for_this_environment` to `workspace.rs`; #6 moved the
URL rewrite and the local-path rule into `remote::Remote`; #7 changed
`LaunchSpec::for_round`'s inputs. The building agent checks every line
reference again on its base.

| Fact | Where (at `smnununx`) |
|---|---|
| The trait's doc says what the bit implies "is decided in" three other places | `runner/mod.rs:147-150` |
| Collect secrets for the round | `daemon/mod.rs:48` |
| Refuse a local-path remote | `daemon/submit.rs:52`, `runner/mod.rs:340` (`reasons_a_container_cannot_run`) |
| Rewrite the URL (HTTPS, or `file://` for a local path) | `runner/mod.rs:59` (inside `Remote` after #6) |
| Add `--provision-toolchain` | `runner/mod.rs:82` |
| The other side of the same policy: `LocalRunner` ignores secrets and strips `GIT_TOKEN_VAR`; `run` picks its credential helper by whether `GIT_TOKEN_VAR` is set | `runner/local.rs:45-96`, `workspace.rs` (after #4) |
| Implementations: `LocalRunner` (false), `DockerRunner` (true), `KubernetesRunner` (true); fakes `RunnerReporting<const IN_A_CONTAINER: bool>`, `LaunchingUntilGivenUp` (false) | `runner/*`, `tests/runner.rs:22`, `tests/daemon_jobs.rs:1020` |

## Success criteria

1. **Named facts.** `runner::RoundEnvironment { reaches_remotes: Reach,
   credentials: Credentials, toolchain: Toolchain }`, each field an enum
   with named variants (for example `Reach::{ThisMachine, FromAnywhere}`,
   `Credentials::{Hosts, PassedIn}`, `Toolchain::{Hosts, Provisioned}`).
   `Runner` has `const ENVIRONMENT: RoundEnvironment`, and each runner
   states all three.
2. **One fact per decision.**
   - Collecting secrets reads `credentials`.
   - Refusing a local-path remote, and the URL form the launch passes,
     read `reaches_remotes`.
   - `--provision-toolchain` reads `toolchain`.
   - `RUNS_IN_A_CONTAINER` is deleted; a grep finds it nowhere.
   - The trait's doc says what each fact decides, not where to look.
3. **`run`'s side stays.** `run` still chooses its credential helper by
   whether `GIT_TOKEN_VAR` is set; its doc says that this is how
   `Credentials::PassedIn` reaches the round. `LocalRunner`'s strip of the
   token stays.
4. **No behaviour change.** Each existing runner's four outcomes are the
   same as today. Tests show it per runner (through the fakes and the
   runners' public behaviour).
5. `just check` is clean and the test count does not fall.
   `just verify-stack` passes after the merge (the supervisor runs it).

## In scope

- `src/runner/mod.rs`, `src/runner/local.rs`, `src/runner/docker.rs`,
  `src/runner/kubernetes.rs` (the const only), `src/daemon/mod.rs`,
  `src/daemon/submit.rs`, `src/remote.rs` (the launch-form input), and
  their tests, including the fakes.

## Out of scope

- An explicit flag telling `run` its credentials (rejected in Q3).
- `exec.rs`'s rule that the agent never sees credentials.
- `LaunchSpec.frames_file` ("container runners ignore it"): leave it.
- #10 builds at the same time in `git.rs`, `run.rs`, `workspace.rs`,
  `delivery.rs`, `payload.rs`, `claim.rs`, and the `--ref` line of
  `LaunchSpec::for_round`. The expected conflict is in `for_round`. Keep
  #8's change there to the `--provision-toolchain` line and the
  launch-form input. If more is needed, stop and ask.

## Constraints

- Everything in `CLAUDE.md`, including one jj change that builds, tests,
  lints and formats on its own; no new trait (the `Runner` seam exists
  with three implementations); tests through the public API.
- A struct cannot be a const generic parameter on stable Rust, so the
  fakes' `const IN_A_CONTAINER: bool` needs another form (for example a
  fake per environment, or a bool mapped to a `RoundEnvironment`). The
  form is the agent's choice.

## Decisions

| # | Decision | By |
|---|---|---|
| 1 | Do #8 now, in wave 4 | user (Q1) |
| 2 | `const ENVIRONMENT: RoundEnvironment` with three named facts | user (Q2) |
| 3 | `run` keeps the `GIT_TOKEN_VAR` rule; only its docs tie it to `Credentials::PassedIn` | user (Q3) |
| 4 | Variant names, and the fakes' form | delegated |
| 5 | Criterion 3's doc on `workspace::credential_helper_for_this_environment` (in #10's file) is written by the supervisor at the merge, in this change (option C) | user (mid-build) |
| 6 | `CLAUDE.md`'s Naming example names `Reach::reasons_it_cannot_reach` instead of the deleted `runner::reasons_a_container_cannot_run`; the supervisor edits it at the merge | user (review) |
| 7 | `LaunchSpec::for_round`'s doc is written in terms of `reaches_remotes` and `toolchain` at the merge; `Reach` stays in `runner` (the `remote`↔`runner` import cycle is accepted) | user (review) |

## Assumptions

- No runner today needs a mix the three facts cannot express (Docker and
  Kubernetes are the same on all three; Local is the opposite on all
  three).

## Open questions

None.

## Implementation notes

Order of work: tests first, then code.

1. **Tests.** Per runner, assert the four outcomes as today (secrets,
   local-path refusal, launch URL form, `--provision-toolchain`); they
   must pass before and after.
2. **Code.** `RoundEnvironment` and its enums; each runner's const; then
   each reader; then delete the bit and fix the trait doc.
3. **Docs.** Update this brief's status line. Do not edit the handoff: the
   supervisor updates it after the merge. Apply the de-ai standard to the
   added lines.
4. **Checks.** `just check` in the workspace. Record the test count in the
   commit body.

## jj

#8 is wave 4 of the handoff's "Waves", after #10 in merge order. Build in
the working-copy change of the jj workspace the supervisor made for it.
Describe it as:

```
refactor(runner): a runner states what it requires, not one bit

<why, and: Tests: N (was M).>
```
