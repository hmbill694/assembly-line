# Arch 01 — a round's number is assigned when it is requested

Finding #1 of `.architecture-reports/2026-10-17-architecture-review.html`,
grilled on 2026-10-17/20. **Status:** approved 2026-10-20; implemented.

## Goal

`JobReport.rounds` becomes the number of the job's latest round, given when
the round is requested rather than when it starts. The daemon stops adding
one to it, and a round cancelled before it starts gets a number of its own.

## Success criteria

1. A revise cancelled before it starts reads
   `job N: failed (round 2) — cancelled before it started`. A first round
   cancelled before it starts reads `failed (round 1) — …`; today it reads
   `failed — …`. A queued revise reads `queued (round 2)`, not
   `queued (round 1)`: the round it waits on is round 2.
2. `status` prints `logs: assembly logs N` only once some round has started,
   so a job cancelled before its first round never points at a `logs`
   command that has nothing to show.
3. No daemon site works out a round number: no `rounds + 1` remains in
   `src/daemon/dispatch.rs` (`is_awaited_by`, `register_if_still_queued`).
   The sites may still `match` on `JobState`.
4. `tests/report.rs` gains event-fed tests for the numbering rule:
   - a first round cancelled while queued is round 1;
   - cancel-then-revise makes the revise round 2;
   - a recorded `RoundStarted` above the request count is a floor;
   - the summary wording from criterion 1.

   `tests/daemon_jobs.rs` gains one end-to-end test: cancel a queued job,
   revise it, and check that the revise's `RoundStarted` says 2.
5. `cargo test`, `cargo clippy --all-targets -- -D warnings` and
   `cargo fmt --check` are clean.

## In scope

- `src/report.rs`: the fold rule and the `rounds` doc comment.
- `src/daemon/dispatch.rs`: `is_awaited_by` and `register_if_still_queued`
  drop `+ 1`.
- `src/daemon/fleet.rs`: unchanged in code; reread to confirm.
- The tests that assert the old numbering:
  - `tests/daemon_jobs.rs:445`, `:491` and `:552` use `rounds == 0` as
    evidence that a cancelled job never started. They change to asserting
    that the log holds no `RoundStarted`.
  - Any `tests/report.rs` case that folds a cancelled request.

## Out of scope

- No event schema change. `RoundStarted { round }` stays, and the collector
  keeps writing it.
- #2, the delivering phase: `fleet.rs`'s Passed/Failed rule is untouched.
- #7, the latest request as one value.
- #5, `JobAddress` / `JobPaths`, and `JobReport.id: u64` → `JobId`.
- Replacing `JobState` with a richer enum (considered and rejected in Q4).
- `Cancelling.was: JobState` in `src/daemon/api.rs`.

## Constraints

- `JobReport::from_events` stays the only fold, and pure.
- Existing `events.jsonl` files fold without error.
- One jj change, building, testing, linting and formatting on its own.
- CLAUDE.md style: functional, descriptive names, no `src/` unit tests.

## Decisions

| # | Decision | By |
|---|---|---|
| 1 | Work through the findings one at a time: grill one, build it, then move on | user |
| 2 | Done = the three checks above. The round-1 wording change for a never-started first job is accepted | user |
| 3 | Scope as above, including no schema change | user |
| 4 | Keep `JobState`, `rounds` and `launched` as they are and change what `rounds` means (option C). Do not introduce a round-phase enum, and revisit that only if #2 adds a sixth state | user |
| 5 | Accept the upgrade risk below | user |
| 6 | C′: `RoundRequested` sets `rounds + 1`; `RoundStarted { round }` sets `max(rounds, round)`. Requests number the rounds, and a recorded start is only a floor against lost log lines | user |
| 7 | The field keeps the name `rounds` ("how many rounds this job has had", now including cancelled ones) | Claude, stated during Q5 |
| 8 | `is_awaited_by` stays private. Cancel and dispatch are covered by the fold tests plus one end-to-end daemon test, not by exposing the method | Claude |
| 9 | The fold also records whether any round has started (`any_round_started`; not `output_captured`, since a round can start and fail before its output log exists), and `status`'s logs line keys on that instead of `rounds > 0`. It is a flag, not a second round number | user, raised during the build |

## Accepted risk

A log written before this change that contains cancel-then-revise, with the
revise still running when the daemon is upgraded: reattach looks for
`round-2.position` where the old daemon wrote `round-1.position`, finds
none, and re-reads that round from the start. Both its output lines and its
already-collected events are appended a second time: `events.jsonl` gains
duplicate `RoundCommitted`/`BranchPushed`/`PullRequestOpened` lines (shown
by a throwaway test during review), and `assembly logs` shows the lines
twice. The fold keeps the last of each, so `status` is unaffected. A
verdict cannot be duplicated, because a round whose verdict was collected is
not reattached.

## Assumptions

- `run` writes no `RoundRequested` and no `RoundStarted` (P1;
  `src/daemon/submit.rs:108` is the only writer of requests), so `run`'s own
  conclusion report keeps `rounds == 0`.
- Collector tests that feed `RoundStarted` without a request keep their
  numbers through the floor.

## Open questions

None.
