# Arch 11 — the Kubernetes reconnect policy is a pure value

Finding #11 of `.architecture-reports/2026-10-17-architecture-review.html`,
grilled with the user on 2026-10-29. **Status:** approved by the user
on 2026-10-29; built and reviewed. Picked up from
`docs/superpowers/plans/2026-10-20-architecture-handoff.md`.

## Goal

The decision to reconnect to a pod's log, drain it, end, or abandon it is
a pure value, `ReconnectPolicy`, tested with no clock and no subprocess.
`KubernetesRound` keeps only the I/O: it measures time, sleeps, spawns
`kubectl`, and writes the abandon message. Today's rules do not change.

## Why this shape

Evidence checked against `knyktlrn` on 2026-10-29.

| Fact | Where |
|---|---|
| `next_line` resets `fruitless_reconnects` when `following_since.elapsed() >= live_connection`, calls `stream_after_follow_ended`, increments the count on `Following`, and records an abandon on `Err` | `src/runner/kubernetes.rs:699-737` |
| `stream_after_follow_ended` both decides (abandon at `FRUITLESS_RECONNECTS_ALLOWED` while running) and does I/O (sleep `poll_interval`, spawn `logs(true)` / `logs(false)`) | `src/runner/kubernetes.rs:641` |
| `text_unless_replayed` resets the count on a new line: a third mutation site | `src/runner/kubernetes.rs:621-636` |
| `following_since` is also set in `create_and_follow` and `reattach` | `src/runner/kubernetes.rs` |
| `a_quiet_pod_whose_connection_keeps_dropping_is_not_abandoned` races `live_connection = 100ms` against a fake whose `logs -f` sleeps 0.2s | `tests/kubernetes_runner.rs:645-682` |
| The cancel test sleeps 300ms before it cancels; it does not prove that a pending pod poll is interrupted | `tests/kubernetes_runner.rs:684-726`, F3 "Known open" |
| The model to copy: `PodProgress`/`pod_progress`, `split_timestamp`, `LogReadPosition::after_line(self, stamp) -> (Self, bool)`, with pure tests | `src/runner/kubernetes.rs:38-256` |

## Success criteria

1. **The policy is a pure value.** `ReconnectPolicy` holds the count of
   fruitless reconnects and the limits. It has two methods:
   - `after_new_line(self) -> Self`: the count is 0;
   - `after_follow_ended(self, progress: &PodProgress, held_for: Duration)
     -> (Self, StreamAction)`, with
     `StreamAction { Reconnect, Drain, End, Abandon }`.
   No other code in `kubernetes.rs` reads or writes the count.
2. **Today's rules, exactly.**
   - Running, and the follow held `live_connection` or longer: the count
     is reset to 0 before the check.
   - Running, and the count is at `FRUITLESS_RECONNECTS_ALLOWED` (5):
     `Abandon`.
   - Running otherwise: the count rises by 1, `Reconnect`.
   - Finished: `Drain`. Waiting or gone: `End`.
   - A new line resets the count to 0.
   - A `kubectl` error from the pod poll, or a failed spawn of the next
     `logs`, still abandons the stream with that error's text. Those are
     I/O results, outside the policy.
   - The abandon message is unchanged ("`kubectl logs` kept ending with no
     output while the pod ran", plus the last unstamped line).
3. **Pure tests.** Tests in `tests/kubernetes_runner.rs` drive
   `ReconnectPolicy` with `PodProgress` values and `Duration`s, with no
   clock and no subprocess. One test per rule in criterion 2 (the policy
   rules, not the I/O errors), plus a test that 5 fruitless follows then a
   new line then 5 more are not abandoned.
4. **No wall-clock races.**
   - `a_quiet_pod_whose_connection_keeps_dropping_is_not_abandoned` is
     replaced by a test with `live_connection = Duration::ZERO`: every
     follow counts as live. The fake drops the connection more than 5
     times with no output, then finishes the pod. The round is not
     abandoned. No timing margin.
   - `a_log_stream_that_never_yields…` and `a_log_stream_that_only_replays…`
     stay as the wiring checks for `Abandon`.
   - The cancel test waits until the fake's argv log holds `get pods`, then
     cancels. No fixed sleep.
   - Amendment (decision 7): one wiring test for `held_for`, with
     `live_connection = 1ms` and a `logs -f` fake that sleeps 0.2s before
     it ends, proves that `next_line` passes the real time a follow held.
     Its margin is one-sided (it fails falsely only on a stall of about
     199ms), and the user accepted it.
5. `just check` is clean and the test count does not fall below 373 (it
   should rise). `just verify-stack` passes after the merge (the
   supervisor runs it).

## In scope

- `src/runner/kubernetes.rs`: `ReconnectPolicy`, `StreamAction`, and
  `next_line` / `stream_after_follow_ended` / `text_unless_replayed`
  rewritten to use them. `following_since` stays in the I/O code, which
  computes `held_for` from it.
- `tests/kubernetes_runner.rs`: the tests in criteria 3 and 4.
- `docs/superpowers/plans/2026-09-28-software-factory-f3.md`: mark the
  300ms cancel-test known-open item as fixed by this change.

## Out of scope

- Any change to the rules, the constants, or the abandon message.
- `PodProgress`, `LogReadPosition`, `split_timestamp`, the `kubectl`
  commands and their arguments.
- Every file outside the three above. `src/runner/mod.rs` and
  `paths::JobPaths` must not change: #5 builds in `paths.rs` and the
  daemon at the same time, and #4 in `run.rs`, `round.rs`, `delivery.rs`,
  `workspace.rs`. If one must change, stop and ask.

## Constraints

- Everything in `CLAUDE.md`, including one jj change that builds, tests,
  lints and formats on its own; tests through the public API (the policy
  is `pub`, as `LogReadPosition` is); no new `#[cfg(test)]` in `src/`;
  shell-script fakes only; concurrency and timing proven with observable
  evidence, not wall-clock bounds.

## Decisions

| # | Decision | By |
|---|---|---|
| 1 | A pure policy with today's exact rules; no behaviour change | user (Q1) |
| 2 | Shape: `ReconnectPolicy` value with `after_new_line` and `after_follow_ended(progress, held_for) -> (Self, StreamAction)`; time is a `Duration` input; I/O errors and the abandon message stay in the I/O code | user (Q2) |
| 3 | The racy quiet-pod test goes; the never-yields and only-replays tests stay as wiring checks | user (Q3) |
| 4 | The cancel test waits for `get pods` in argv, in this change | user (Q4) |
| 5 | The quiet-pod test is replaced by a deterministic one (`live_connection = 0`) | user (Q5) |
| 6 | Exact names of fields and helpers not named above; whether `held_for` is an `Option` to keep today's "no `following_since`, no reset" edge | delegated |
| 7 | Add a `held_for` wiring test with `live_connection = 1ms` and a 0.2s fake follow (review finding 1, option B); the one-sided margin is accepted | user (mid-build) |

## Assumptions

- Where `following_since` is `None` while following, today's code does not
  reset the count. The new code keeps that edge (for example with
  `held_for: Option<Duration>`, or by passing `Duration::ZERO`). The agent
  checks which is equivalent.
- `live_connection` and `poll_interval` stay settable on the runner for
  tests, as today.

## Open questions

None.

## Implementation notes

Order of work: tests first, then code.

1. **Tests.** The pure policy tests (criterion 3), which fail to compile
   first; the deterministic quiet-pod test; the cancel test change.
2. **Code.** `ReconnectPolicy` and `StreamAction` next to
   `LogReadPosition`; then `next_line` and `stream_after_follow_ended` use
   them; `text_unless_replayed` calls `after_new_line`.
3. **Docs.** Update this brief's status line and the F3 known-open item.
   Do not edit the handoff: the supervisor updates it after the merge.
   Apply the de-ai standard to the added lines.
4. **Checks.** `just check` in the workspace. Record the test count in the
   commit body.

## jj

#11 is in wave 1 of the handoff's "Waves". Build in the working-copy
change of the jj workspace the supervisor made for it. The supervisor puts
it in the stack at the merge. Describe it as:

```
jj describe -m "refactor(kubernetes): the reconnect policy is a pure value

<why, and: Tests: N (was 373).>"
```
