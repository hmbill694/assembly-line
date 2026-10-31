# Arch 10 — commits, pinned refs and job branches as values

Finding #10 of `.architecture-reports/2026-10-17-architecture-review.html`,
grilled with the user on 2026-10-29. **Status:** built and reviewed.
Picked up from
`docs/superpowers/plans/2026-10-20-architecture-handoff.md`.

## Goal

A commit id, a pinned ref and a job's branch are values, not `&str`, so a
call with its arguments swapped no longer compiles. `PinnedRef` owns its
`REF@SHA` text form, both writing and parsing it.

## Why this shape

Evidence checked at `smnununx`. Waves 1–3 (#4, #5, #11, #9, #7, #6) moved
code in `run.rs`, `workspace.rs`, `delivery.rs`, `runner/mod.rs`, `git.rs`
and the daemon, so the building agent checks every line reference again on
its base. #6 added `remote::Remote` and renamed `git.rs`'s remote
parameters to `name_or_url: &str`.

| Fact | Where (at `smnununx`) |
|---|---|
| Opposite argument orders: `check_out_new_branch(clone, branch, at)` vs `create_branch_if_absent(repo, remote, sha, branch)` | `git.rs:462`, `:542` |
| Many functions take two or more `&str` of different kinds: `sha_on_remote`, `fetched_sha`, `remote_lacks_ref`, `remote_branches_matching`, `pinned`, `push_head_as`, `file_at_ref`, `commit_is_ahead_of`; outside `git.rs`, `workspace::pin_in_clone`, `workspace::start_round`, `delivery::deliver` | `git.rs`, `workspace.rs`, `delivery.rs` |
| `REF@SHA` is written in `runner/mod.rs` (`format!("--ref={}@{}", …)`) and parsed by `run.rs`'s private `split_pin` (a suffix of 40 or 64 hex is a pin); `PinnedRef` has no text form | `runner/mod.rs:75`, `run.rs:439-449`, `git.rs:235` |
| The 12-character abbreviation is copied twice | `run.rs:246`, `submission.rs:43` |
| A job's branch travels as a `String` derived from a `JobId`: `RoundPayload.branch`, `RoundWorkspace.branch`, `job_branch_in_clone`, `delivery::deliver`, `Delivered::Pushed`, `claim.rs:35`, `submit.rs:168` | various |
| Wire formats that hold a branch string: `EventKind::BranchPushed.branch`, `api::Queued.branch` | `event.rs:44`, `api.rs:43` |
| `git::commit_is_ahead_of` is `pub`, with one caller in `git.rs` (after #4) | `git.rs:619` |
| `read_events` skips any line that fails to parse | `event.rs` |

## Success criteria

1. **`CommitId`.** A newtype in `git.rs`.
   - `CommitId::from_str` accepts 40 or 64 hex characters and refuses the
     rest with an error that says what to do. Git output and the SHA in
     `--ref REF@SHA` are parsed with it.
   - Serde is transparent with no check, so every existing log line still
     reads. A test pins that an event whose SHA is not 40/64 hex still
     reads and folds.
   - `CommitId::short()` gives the first 12 characters. The two copies
     (`run.rs`, `submission.rs`) use it; their output does not change.
   - `PinnedRef.sha` is a `CommitId`. Functions that take or return a SHA
     use `CommitId`.
2. **`PinnedRef` owns `REF@SHA`.** `Display` writes `name@sha`; `FromStr`
   parses it by the same rule as today's `split_pin` (a suffix that is a
   valid `CommitId` is a pin; otherwise the whole text is a ref name).
   `runner/mod.rs` writes `--ref` with `Display`; `split_pin` is deleted.
   A round-trip test covers names that hold `@`.
3. **Job branches.** Internal APIs that mean "this job's branch" take a
   `JobId` and call `branch_name()` inside: `RoundPayload`,
   `RoundWorkspace`, `job_branch_in_clone`, `delivery::deliver` and its
   helpers, `claim.rs`, `submit.rs`. `BranchPushed.branch` and
   `api::Queued.branch` stay strings on the wire.
4. **`git::commit_is_ahead_of` is private.**
5. **Nothing else changes.** Output text, the event schema, the launch
   argv, and the CLI's accepted `--ref` forms.
6. `just check` is clean and the test count rises.
   `just verify-stack` passes after the merge (the supervisor runs it).

## In scope

- `src/git.rs`, `src/run.rs`, `src/runner/mod.rs`, `src/submission.rs`,
  `src/workspace.rs`, `src/delivery.rs`, `src/payload.rs`, `src/round.rs`,
  `src/claim.rs`, `src/daemon/submit.rs`, and other callers the compiler
  names, with their tests.

## Out of scope

- Remote parameters (`name_or_url: &str`): #6 settled them.
- `RUNS_IN_A_CONTAINER` and what it decides: #8 builds at the same time in
  `runner/mod.rs`, `daemon/mod.rs`, `daemon/submit.rs` and `runner/*`. The
  expected conflict is the `--ref` line and nearby lines of
  `LaunchSpec::for_round`. Keep #10's change to `for_round` to the `--ref`
  text. If #10 must change more of `for_round`, stop and ask.
- Wire formats (`BranchPushed.branch`, `api::Queued.branch`).

## Constraints

- Everything in `CLAUDE.md`, including one jj change that builds, tests,
  lints and formats on its own; tests through the public API; error types
  say what to do. Job ids are never user input; `JobId::branch_name` is the
  one place a job's branch name is made.

## Decisions

| # | Decision | By |
|---|---|---|
| 1 | All four: `PinnedRef` text form, `CommitId` with `short()`, `JobId` for job branches internally, `commit_is_ahead_of` private | user (Q1; the last from #4's review) |
| 2 | `CommitId` validates on parse (40/64 hex), not in serde; old logs still read | user (Q2) |
| 3 | Argument order in `git.rs`: the agent may align the two branch-creating functions; with distinct types, order is not load-bearing | delegated |

## Assumptions

- Every SHA git hands back is 40 or 64 hex characters. If one is not,
  parsing it is an error that names it, not a silent pass.
- `short()` stays 12 characters.

## Open questions

None.

## Implementation notes

Order of work: tests first, then code.

1. **Tests.** `CommitId` parsing (valid 40, valid 64, wrong length, not
   hex); `PinnedRef` round-trip (including `@` in the name, and a suffix
   that is not a SHA); an event with an odd SHA still reads and folds.
2. **Code.** `CommitId`; `PinnedRef`'s `Display`/`FromStr`; callers of
   SHAs; then the job-branch parameters; then `commit_is_ahead_of`.
3. **Docs.** Update this brief's status line. Do not edit the handoff: the
   supervisor updates it after the merge. Apply the de-ai standard to the
   added lines.
4. **Checks.** `just check` in the workspace. Record the test count in the
   commit body.

## jj

#10 is wave 4 of the handoff's "Waves", on the merged waves 1–3, before
#8 in merge order. Build in the working-copy change of the jj workspace
the supervisor made for it. Describe it as:

```
refactor(git): commits, pinned refs and job branches are values

<why, and: Tests: N (was M).>
```
