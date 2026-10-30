# Arch 04 — a round owns its life and hands back what it left

Finding #4 of `.architecture-reports/2026-10-17-architecture-review.html`,
grilled with the user on 2026-10-29. **Status:** built and reviewed. Picked up from
`docs/superpowers/plans/2026-10-20-architecture-handoff.md`.

## Goal

A round owns its whole life, from the start of its workspace to the
discard of its clone. It ends in a value that carries its verdict and the
branch tip it left. `run` becomes three steps: run the round, deliver what
it left, close it. Whether a round has work to deliver is decided by a pure
function that cannot fail silently.

## Why this shape

| Fact | Where |
|---|---|
| `run_round_in` returns only a `RoundClosing` (verdict and closing events). Its `RoundResult` knows the commit and the push, but the caller does not get them | `src/round.rs:142` |
| `run.rs` rebuilds the tip by scanning frames for the last `RoundCommitted`, then asks git whether it is ahead of the base | `src/run.rs`, `branch_carries_work` |
| `commit_is_ahead_of(..).unwrap_or(false)` turns a git error into "no work", so delivery is skipped without a word | same |
| `PROVISIONING_LIMIT`, `within_provisioning_limit`, `credential_helper_for_this_environment` and `discard_reporting_failure` are in `round.rs` but used only by `run.rs` | `src/round.rs`, `src/run.rs:19-21` |
| `run.rs` writes the `RoundFailed` for a failed `start_round`; `round.rs` writes every other one | `src/run.rs`, `ReadyJob::run` |
| `events_for_completion` says it is pure "so the ordering … can be asserted", but it is private and no test asserts the order of a rejected or failed round | `src/round.rs:408-414`, `tests/verify.rs` |
| The delivery decisions (is there work, which branch the pull request targets, `BaseDiffers`) can only be reached by running the binary with a `gh` fake | `src/run.rs` |
| #2 fixed the order: work events, delivery (`gh` runs inside the clone), verdict, discard | `2026-10-20-arch-02-verdict-after-delivery.md` |
| The clone is made in `prepare_run`, before admission, because `run` reads the config from it (#3). So the round cannot own the clone | `src/run.rs`, `prepare_run` |

## Success criteria

1. **The round hands back what it left.** `round::run_round` returns a
   `FinishedRound` that carries the verdict and the branch tip the round
   left on the remote. Nothing in `run.rs` reads frames to learn what the
   round did. The job's final report still reads frames; that is a
   different thing.
2. **Deciding whether there is work cannot fail.** It makes no git call:
   a passing round carries work when its tip differs from the base's SHA.
3. **The round owns its life from start to discard.** `run.rs` no longer
   calls `workspace::start_round`, no longer writes a `RoundFailed` for a
   failed start, and no longer calls `discard_reporting_failure`.
   `FinishedRound` does all three. Delivery still runs between the round's
   work and its verdict.
4. **The clone's helpers live with the clone.** `PROVISIONING_LIMIT`,
   `within_provisioning_limit` and `credential_helper_for_this_environment`
   move to `workspace.rs`. A grep shows that `round.rs` exports nothing that
   only `run.rs` uses.
5. **The delivery decision is pure and tested.**
   - `delivery::delivery_plan` decides from the verdict, the tip, the base
     and the configured target whether to deliver, which branch the pull
     request targets, and whether a `BaseDiffers` note is due.
   - Tests in `tests/delivery.rs` call it with no process: a failed round,
     a tip equal to the base, a configured target that differs from the
     base, and the default target.
   - The order of the closing events is tested through `Harness`, with an
     exact slice match, for a round `verify` rejects and for an agent that
     fails after it changed files.
6. **Nothing else changes.** The event schema, the event order, the frame
   texts and the exit codes stay the same, except for the stated behaviour
   change below.
7. `just check` is clean and the test count rises from 373.
   `just verify-stack` passes after the merge (the supervisor runs it).

## Stated behaviour change

- When a revise changes nothing, and its branch is already merged into a
  newer base or never got a commit, its tip differs from the newly pinned
  base. `gh pr create` now runs and refuses ("no commits between …"), and
  its refusal shows as the usual output line:
  `pushed al/job-N — no pull request opened: <gh's reason>`. Today git
  says "not ahead" and nothing is attempted.
- A git error can no longer skip delivery silently, because no git call
  is left to fail.

### Amendment: decision 9

Found by the building agent's review; decided by the user on 2026-10-29
(option A: accept it and state it, no code change).

- With a configured pull request target that differs from the base, a
  revise that changes nothing, after its base moved, now runs
  `gh pr create --base <target> --head al/job-N`. If the target lacks
  commits that the old base has, GitHub can open a pull request that
  holds only base commits and none of the job's work. The `BaseDiffers`
  note prints with it. The `gh` fakes cannot test GitHub's side.
- The same path gives two new output lines for such a revise:
  - with delivery mode `none`: `not delivered: delivery mode is "none"`;
  - with no `gh`: `pushed al/job-N — no pull request opened: …`.
- Rejected: comparing against the commit the job was claimed at (needs a
  git call or a new payload or event field), and keeping the git ancestry
  check for this case only (a git call that can fail, rejected in Q4).

## In scope

- **`src/round.rs`.**
  - `run_round(clone, payload, frames, provisioning_deadline, cancel) ->
    anyhow::Result<FinishedRound>` calls `workspace::start_round` itself.
    A failed start gives a `FinishedRound` with a failed verdict, its
    reason, and no workspace.
  - `FinishedRound` holds the verdict, the branch tip (the round's own
    commit, otherwise its start), the open workspace if there is one, and
    the closing events, unwritten. Methods:
    - `verdict()`;
    - `branch_tip()`;
    - `workspace_path()`, where `gh` runs;
    - `failed_because(reason)`, which replaces the verdict, for a cancel
      during delivery;
    - `close(self, frames) -> io::Result<Verdict>`, which writes the
      closing events and then discards the clone. It is the only way to
      end a round on purpose.
  - `run_round_in`, `RoundClosing` and `discard_reporting_failure` stop
    being public.
  - `events_for_completion`'s doc says why the order matters for replay,
    not that it "can be asserted".
- **`src/delivery.rs`.**
  - `DeliveryPlan { NothingToDeliver, OpenPullRequest { target,
    base_differs } }` and `delivery_plan(verdict, branch_tip, base,
    pull_request_target)`.
  - `BaseDiffers` and its `Display` move here from `run.rs`.
  - Delivery mode `none` stays inside `delivery::deliver`, so "not
    delivered: delivery mode is \"none\"" is unchanged.
- **`src/run.rs`.**
  - `ReadyJob::run`: `run_round`, then match on `delivery_plan`, then
    deliver, then the cancel rule, then `close`.
  - `branch_carries_work` and the frame scan are deleted.
  - `prepare_run` calls one `workspace` function that clones within the
    budget and returns the clone and its provisioning deadline.
- **`src/workspace.rs`.** Receives the clone helpers and that function.
- **Tests:** the new tests from criterion 5, and the changes existing
  tests need.

## Out of scope

- #9 renames (`ReadyJob`, `JobConclusion`, `run_whole_job`,
  `RoundRequest`). They come next, as a change of their own.
- #6 (`DEFAULT_REMOTE` vs `CLONE_REMOTE` inside the clone) and #10 (typed
  SHAs and refs).
- `delivery.rs`'s `gh` calls and their cancel behaviour from #2.
- The event schema, the collector, the fold and the runners. If one of
  them must change, the design is wrong: stop and ask the user.
- The `#[cfg(test)]` provisioning test in `round.rs`. `provision_toolchain`
  stays in `round.rs`, so the test stays too.

## Constraints

- Everything in `CLAUDE.md`, including:
  - one jj change that builds, tests, lints and formats on its own;
  - no new `#[cfg(test)]` in `src/`;
  - shell-script fakes only;
  - test through the public API.
- #2's order holds: work events, delivery, verdict, discard.
- `JobReport::from_events` does not change. Old logs still fold.

## Decisions

| # | Decision | By |
|---|---|---|
| 1 | Solve all four problems: lost work, silent skip, split ownership, hard testing | user (Q1) |
| 2 | Done = criteria 1–7 | user (Q2) |
| 3 | Scope as above; #9 renames are a separate change straight after | user (Q3) |
| 4 | "Carries work" is `tip != base.sha`, with no git call (option A). The user rejected B, which kept the git check and printed its failure | user (Q4) |
| 5 | `run_round` returns a `FinishedRound` that holds the clone until `close`, which writes the verdict and discards | user (Q5) |
| 6 | `delivery::delivery_plan` returns a `DeliveryPlan`; `BaseDiffers` moves to `delivery.rs` | user (Q6) |
| 7 | The order of closing events is tested through `Harness`; `events_for_completion` stays private | user (Q7) |
| 8 | The clone helpers move to `workspace.rs`, behind one function that clones within the budget and returns the deadline | delegated |
| 9 | Accept and state the case where a moved base and a different pull request target open a pull request with no job work (see "Amendment: decision 9") | user (mid-build) |

## Assumptions

- The job's branch always descends from its base: it is claimed at the
  base's SHA, and every round pushes on top of its start. A branch that a
  person resets to something unrelated may open a pull request that `gh`
  refuses. That refusal shows as output.
- An early `?` in `ReadyJob::run` (a frame that cannot be written) leaves
  the clone undiscarded. That is true today as well.

## Open questions

None.

## Implementation notes

Order of work: tests first, then code.

1. **Tests.**
   - `tests/delivery.rs`: the four pure `delivery_plan` tests.
   - `tests/verify.rs` or `tests/round.rs`: exact slice matches for a
     rejected round and for a failed round that left work.
   - Find a revise test that delivers an earlier round's work, and check
     that it still delivers.
2. **Code**, in the order given in "In scope".
3. **Docs.** Update this brief's status line. Do not edit the handoff:
   the supervisor updates it after the merge. Apply the de-ai standard to
   the added lines.
4. **Checks.** `just check` in the workspace. The supervisor runs
   `just verify-stack` in the main workspace after the merge. Record the
   test count in the commit body.

## jj

#4 is in wave 1 of the handoff's "Waves". Build in the working-copy change
of the jj workspace the supervisor made for it, on `knyktlrn`. The
supervisor puts it in the stack at the merge. Describe it as:

```
jj describe -m "refactor(round): a round owns its life and hands back what it left

<why, the stated behaviour change, and: Tests: N (was 373).>"
```
