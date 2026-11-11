# Arch 07 — a job's requests are one value

Finding #7 of `.architecture-reports/2026-10-17-architecture-review.html`,
grilled with the user on 2026-10-29. **Status:** built and reviewed.
Picked up from
`docs/superpowers/plans/2026-10-20-architecture-handoff.md`.

## Goal

`JobReport` holds a job's requests as one optional value: its first
request (the job's identity) and its latest (what its next round runs).
Callers match one `Option` instead of re-checking several for a state that
cannot happen, and the launch is made from the requests.

## Why this shape

Evidence checked at `smnununx`. Wave 1 (#4, #5) moved code in `run.rs`,
`paths.rs` and `daemon/*`, so the building agent checks every line
reference again on its base.

| Fact | Where |
|---|---|
| The `RoundRequested` arm sets six `Option`s together: `remote_url` and `first_prompt` from the first request (`or_else`), `base`, `latest_prompt`, `provider`, `requested_at` from the latest | `src/report.rs:114-119` |
| `JobProgress` copies the same fields, and `from_events` copies them out one by one | `src/report.rs:86-93`, `:201-208` |
| dispatch re-checks four of them: `let (Some(remote_url), Some(base), Some(prompt), Some(provider)) = … else bail!("the job has no recorded request to run")` | `src/daemon/dispatch.rs:400-408` |
| submit re-checks two, with a wrong message ("its first round never started") | `src/daemon/submit.rs:162-167` |
| The launch takes seven loose parameters | `src/runner/mod.rs:48-56`, called at `src/daemon/dispatch.rs:435` |
| A revise writes the URL the person typed, which keys to the same job but can be another spelling | `src/daemon/submit.rs:105` |
| The requeue sorts by `requested_at` | `src/daemon/mod.rs:158` |
| No renderer reads any of these fields; `tests/report.rs:605-609` asserts them | `src/report.rs` |
| After #5, fleet takes the key from the directory, not from `remote_url` | #5's change |

## Success criteria

1. **One value.**
   - `report::RoundRequest { remote_url, base: PinnedRef, prompt, provider,
     requested_at: DateTime<Utc> }`: one `RoundRequested` event and when it
     was written.
   - `report::JobRequests { first: RoundRequest, latest: RoundRequest }`.
   - `JobReport.requests: Option<JobRequests>`, set by the first
     `RoundRequested`; each later one replaces `latest` only.
   - The fields `remote_url`, `base`, `first_prompt`, `latest_prompt`,
     `provider` and `requested_at` are gone from `JobReport` and
     `JobProgress`. `JobProgress` holds the same `Option<JobRequests>`.
2. **No re-checks.** dispatch and submit each match one `Option`. The
   requeue sorts by the latest request's `requested_at`.
3. **The launch takes the requests.** `LaunchSpec::for_round` takes the
   job's paths, the round, the `JobRequests`, and the base and settings
   that admission returned (as today since #3), not loose strings. The URL
   comes from the first request and the prompt from the latest, as today.
   The launch's argv is unchanged.
4. **The revise refusal** for a job with no recorded request says what
   happened and what to do, for example "job {id} has no recorded request
   in its event log, so there is nothing to revise; submit without --job
   to start a new job". A test pins it.
5. **Nothing else changes.** The event schema, the fold's other fields,
   every renderer, and the launch's argv. Old logs still fold.
6. `just check` is clean and the test count does not fall (it should rise
   by the refusal test). `just verify-stack` passes after the merge (the
   supervisor runs it).

## Stated behaviour change

- The text of `submit --job`'s refusal for a job with no recorded request.

## In scope

- `src/report.rs`, `src/daemon/dispatch.rs`, `src/daemon/submit.rs`,
  `src/daemon/mod.rs`, `src/runner/mod.rs` (`LaunchSpec::for_round`), and
  their tests (`tests/report.rs`, `tests/runner.rs`, `tests/support/`,
  daemon tests as needed).

## Out of scope

- The event schema (`EventKind::RoundRequested` keeps its fields).
- Which URL the launch uses (still the first request's). Rewriting the URL
  for containers stays in `for_round` as it is: #6 and #8 own that.
- `src/run.rs`, `src/main.rs`, `src/cli.rs`, `src/payload.rs`: #9 builds
  there at the same time, and renames `payload::RoundRequest` to
  `payload::RoundBeforeSettings`. Until the merge, both
  `payload::RoundRequest` and the new `report::RoundRequest` exist; refer
  to the new one by its path where both are in scope. If #7 must change
  one of #9's files, stop and ask.

## Constraints

- Everything in `CLAUDE.md`, including one jj change that builds, tests,
  lints and formats on its own; `JobReport::from_events` is the only fold
  and stays pure; nothing that cannot be rebuilt from `events.jsonl` goes
  into `JobReport`.
- #1's rules for `rounds` and `any_round_started` hold.

## Decisions

| # | Decision | By |
|---|---|---|
| 1 | `requests: Option<JobRequests { first, latest }>`, each a `report::RoundRequest`; the launch keeps the first request's URL | user (Q1) |
| 2 | `LaunchSpec::for_round` takes the requests and the admitted base and settings | user (Q2) |
| 3 | Fix the revise refusal text | user (Q3) |
| 4 | `JobProgress` holds the same value; exact parameter shape of `for_round` | delegated |

## Assumptions

- `first_prompt` has no reader in `src/`; it lives on as
  `requests.first.prompt`.
- The dispatcher's "the job has no recorded request to run" branch stays,
  as the one `None` case (a torn or empty log).

## Open questions

None.

## Implementation notes

Order of work: tests first, then code.

1. **Tests.** `tests/report.rs:605-609` asserts `requests.first` and
   `requests.latest`; a test that a second request replaces only `latest`;
   the refusal-text test. They fail first.
2. **Code.** `RoundRequest` and `JobRequests` in `report.rs`; the fold;
   dispatch, submit, the requeue sort; `LaunchSpec::for_round`.
3. **Docs.** Update this brief's status line. Do not edit the handoff: the
   supervisor updates it after the merge. Apply the de-ai standard to the
   added lines.
4. **Checks.** `just check` in the workspace. Record the test count in the
   commit body.

## jj

#7 is in wave 2 of the handoff's "Waves", on the merged wave 1, after #9
in merge order. Build in the working-copy change of the jj workspace the
supervisor made for it. Describe it as:

```
refactor(report): a job's requests are one value

<why, the stated behaviour change, and: Tests: N (was M).>
```
