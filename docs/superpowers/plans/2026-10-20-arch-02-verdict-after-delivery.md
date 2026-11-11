# Arch 02 — a round's verdict is the last thing it reports

Finding #2 of `.architecture-reports/2026-10-17-architecture-review.html`,
grilled with the user on 2026-10-20. **Status:** built and reviewed.
Picked up from `docs/superpowers/plans/2026-10-20-architecture-handoff.md`.

## Goal

A passing round delivers its pull request *before* it reports its verdict.
The job's log then says `Running` for as long as the round still has
something to report, so a daemon restarted mid-delivery reattaches to the
round and collects its `PullRequestOpened`, instead of seeing `Passed` and
deciding the job is owed nothing.

This closes the F3 known-open item "A round with a verdict still delivering
when the daemon stops loses its `PullRequestOpened` event". It also restores
the invariant that the daemon's only memory is its event logs. Today the cap
and the cancel table count delivering rounds that a restarted daemon cannot
know about.

## Why this shape

| Fact | Where |
|---|---|
| `RoundPassed` is written inside `round::run_round_in`, via `record_completion` | `src/round.rs:106-125`, `:356-366` |
| Delivery happens afterwards, in `ReadyJob::run` | `src/run.rs:274-283` |
| A restarted daemon owes nothing to a `Passed` or `Failed` job | `src/daemon/fleet.rs:37` |
| The cancel table already holds rounds past their verdict | `src/daemon/dispatch.rs:77-79` |

Rejected alternative (user, Q1): a new "stream ended" event written by the
collector, with fleet reattaching jobs that have a verdict but no stream
end. It needs a schema change, adds a second meaning of "done", and does
nothing for a hand-typed `run`, which has no collector.

## Success criteria

1. **Event order.** A passing round's stream ends `RoundCommitted,
   BranchPushed, PullRequestOpened, RoundPassed`: the verdict is always the
   last event `run` reports. A test asserts this order. Failed rounds don't
   deliver, and their order doesn't change.
2. **Restart during delivery.** A daemon test with a `gh` fake that stalls
   in `pr create`:
   - the first daemon is stopped mid-delivery;
   - a second daemon starts on the same root and reattaches;
   - the job's log then holds `PullRequestOpened` followed by `RoundPassed`;
   - `assembly status N` prints `pull request: <url>`.
3. **Cancel during delivery.** With the same stalling `gh`, `assembly
   cancel N` ends the job as `failed (round 1) — cancelled while delivering
   its pull request`.
4. **Stated behaviour changes** (all accepted by the user):
   - `took Ns` now includes the time spent delivering;
   - a hand-typed `run` prints `passed` after the pull-request line instead
     of before it;
   - a cancel during delivery is accepted (it used to be refused as "not
     queued or running"), and the round fails as cancelled;
   - a revise submitted during delivery is refused as "already queued or
     running" (it used to be accepted). Found in review; it follows from the
     log saying `Running`.
5. **Docs.**
   - Remove the "Known open" bullet about the lost `PullRequestOpened` from
     `docs/superpowers/plans/2026-09-28-software-factory-f3.md`.
   - Update the `CancelTable` comment at `src/daemon/dispatch.rs:77-79`: the
     only thing a round now does after its verdict is discard its clone.
6. `cargo test`, `cargo clippy --all-targets -- -D warnings` and
   `cargo fmt --check` are clean, and the test count rises from 367.

## In scope

- **`src/round.rs`.**
  - `run_round_in` keeps reporting the *work* events (`RoundCommitted`,
    `BranchPushed`) as they happen.
  - It hands the *closing* events (`VerifyRejected` / `RoundFailed` /
    `RoundPassed`) back to its caller instead of writing them.
  - `events_for_completion` stays the one place that decides which closing
    events a result implies, and in what order.
- **`src/run.rs`, `ReadyJob::run`.**
  - A failed round: write the closing events straight away.
  - A passed round: deliver (when `branch_carries_work`), then write the
    closing events. If `cancel` fired at any point during delivery, write
    `RoundFailed { reason: "cancelled while delivering its pull request" }`
    instead of `RoundPassed`.
  - The clone is discarded after the verdict, as now.
  - The verdict that `JobConclusion` and the exit code carry must match what
    was written.
- The doc and comment updates from criterion 5, and the tests from 1–3.

## Out of scope

- The rest of finding #4: `run_round_in` returning its tip and whether it
  pushed; the frame rescan in `branch_carries_work` and its
  `unwrap_or(false)`; moving the provisioning helpers out of `round.rs`.
- The clone-discard window after the verdict. A restart there loses at most
  one "could not discard" output line.
- `src/daemon/fleet.rs`, the cancel table's logic, and the event schema. If
  any of them needs to change, the design is wrong: stop and ask the user.

## Constraints

- Everything in `CLAUDE.md`, including:
  - one jj change that builds, tests, lints and formats on its own;
  - no `#[cfg(test)]` in `src/`;
  - shell-script fakes, never the real `gh`.
- `JobReport::from_events` must not change: no fold edit is needed.
- The event log stays append-only, and old logs still fold.

## Decisions

| # | Decision | By |
|---|---|---|
| 1 | Move the verdict after delivery (A), not a stream-end event (B) | user |
| 2 | A cancel during delivery is honoured. The round fails with "cancelled while delivering its pull request", the same on every runner | user |
| 3 | Done = criteria 1–6 | user |
| 4 | Scope as above. #2 does only the reorder; the rest of #4 waits | Claude proposed; user confirmed 2026-10-20 |
| 5 | A cancelled round asks `gh` for nothing more, and every passing round that sees a cancel before its verdict fails as cancelled, whether or not it had work to deliver | user, during the build |

## Amendment: decision 5

The review found two gaps in decision 2 as first built:

- The local runner's `killpg` killed a `gh pr view` in flight. Delivery
  read that as "no pull request open" and went on to `gh pr create`, which
  started after the cancel. The job then failed with a live pull request.
- A passing round with no work to deliver skipped the cancel check, so a
  cancel there ended the job `passed`.

The fix:

- `delivery::deliver` takes the cancel token. `gh` runs in a session of its
  own, as `git` does, so only the token reaches it. A cancel kills its
  group, and no further `gh` call starts.
- `run` applies the cancel check to every passing round.

A cancel that lands after `gh pr create` has returned still fails the
round, with its pull request open. That window is accepted.

## Why decision 2 is needed

Under the new order the log says `Running` throughout delivery, so
`JobQueue::cancel` accepts a cancel there. Without a rule, each runner gives
a different result:

- **Kubernetes** deletes the Job (`src/runner/kubernetes.rs:739`). The pod
  dies mid-`gh`, no verdict arrives, and the collector writes `RoundFailed`.
- **Docker** runs `docker stop -t <grace>`. Whether `gh` finishes depends on
  timing.
- **Local** sends `killpg(SIGTERM)`, which also kills `gh` because it is in
  the same process group. `run` traps the signal into `cancel`
  (`src/main.rs:157`) and would otherwise still write `Passed`.

The rule makes `run` itself check `cancel` once delivery returns, so local
and Docker give the same answer as Kubernetes's missing-verdict path. A
Kubernetes pod killed before `run` can write anything still ends in the
collector's "ended without reporting a verdict" failure. That is an accepted
difference in the wording only; the outcome is the same.

## Implementation notes

Order of work: tests first, then code.

1. **The event-order test.** Use `tests/support` `Harness` with a `PATH` `gh`
   fake that succeeds: copy `fake_gh_capturing_args` in
   `tests/delivery.rs:138-176` or move it into `tests/support`. The config
   needs `[delivery] mode = "pr"`, which is the default, and an agent that
   commits (`tests/fixtures/committing-agent.sh`). Assert that the last two
   events are `PullRequestOpened, RoundPassed`.
   - `tests/delivery.rs` and `tests/round.rs` may already assert the old
     order. Grep for `RoundPassed` near `PullRequestOpened` and update those
     assertions.
2. **The daemon tests.**
   - The fixture in `tests/daemon_jobs.rs` writes
     `[delivery] mode = "none"` (`publish_opted_in_repository`, `:142-150`)
     and puts a refusing `gh` on `PATH` (`with_daemon`, `:59-75`). Add a
     variant that keeps delivery on, and pass a stalling `gh` through the
     daemon's `env`; a later `PATH` entry wins.
   - The stalling `gh` can follow `path_where_git_stalls`
     (`tests/daemon_jobs.rs:1205-1229`): write `<stall>.reached` and loop
     while `<stall>` exists, but only for `pr create`. `pr view` should exit
     1 so delivery goes on to `create`.
   - Restart: follow `a_reattached_round_can_be_cancelled_and_counts_under_the_cap`
     (`:899`): `fx.daemon.take().unwrap().stop()`, then `fx.with_daemon(..)`.
     Stopping the daemon stops no round (F3 plan, known-stale list), so the
     local round keeps running and the new daemon reattaches it from its
     frames file.
   - Prove the timing with observable evidence: wait on `<stall>.reached`
     before stopping or cancelling, never a sleep.
3. **The code.** Change `record_completion` / `run_round_in` so the closing
   events come back to the caller, then change `ReadyJob::run`. The early
   `start_round` failure path in `run.rs:264-272` already writes its own
   `RoundFailed` and doesn't deliver, so leave it alone.
4. **Docs and comments** from criterion 5. Run the de-ai standard over the
   added lines.
5. **Checks.** `just check`, about 80s. Record the test count in the commit
   body.

## jj

The brief and the handoff doc are in change `nnzttxxs`, which sits directly
on `vtruquzw` (arch-01). Build in `nnzttxxs` itself (`@` when this was
written), then describe it:

```
jj describe -m "fix(run): a round's verdict is the last thing it reports

<why, the behaviour changes from criterion 4, and: Tests: N (was 367).>"
```

`prwrkokz` above it holds only the untracked architecture report HTML;
leave it there.
