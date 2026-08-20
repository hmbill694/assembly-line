# assembly-line M3 Implementation Plan — stateless jobs

**Status:** in progress
**Spec:** `docs/superpowers/specs/2026-08-15-assembly-line.md`

## The one-line rule

> **A node's branch always survives; a node's worktree never does.**

Everything in this milestone follows from that sentence. M2 had it backwards:
a failed node kept its worktree on the machine that ran it, and the branch was
incidental. That makes a job stateful, which blocks containers (a container
cannot keep a host worktree) and makes deferred review expensive (the thing
you review has to stay on disk until you get to it).

## Why this unblocks the rest

| Consequence | Enables |
|---|---|
| Scratch worktrees | A job is `(repo, ref, prompt) -> branch`, which is already the shape of a `docker run` or a k8s Job (M4) |
| Branch is the artifact | Failure is inspectable from any machine, not just the one that ran it |
| Revise re-seeds from a branch | Deferred review costs nothing — there is no worktree to keep alive (M5) |

## Decisions already made — do not relitigate

- **Both supervision modes stay.** Supervised gates block; unsupervised gates
  *defer* to a review inbox. `--unsupervised` means defer, not ignore.
- **Revise is a new job**, based at the node's branch tip, appending a round to
  the same branch. Not a re-entry into a preserved worktree.
- **The engine keeps the DAG.** What changes later (M6) is the authoring
  surface, not the scheduler.
- **The runner trait waits for M4.** Defining it now would be a single-impl
  trait, which `CLAUDE.md` forbids.
- **Publishing is push-if-remote.** A repo with no remote just keeps the local
  ref. No special case at the call site.

## Stack

Each change builds, tests, lints and formats on its own (`just verify-stack`).

| # | Change | Touches |
|---|---|---|
| 1 | `feat(git): publish a branch to a remote` | `src/git.rs`, `tests/git.rs` |
| 2 | `refactor(scheduler): a node's worktree is scratch` | `src/scheduler.rs`, `src/workspace.rs`, `tests/agent_nodes.rs` |
| 3 | `feat(scheduler): a failed node publishes what it left` | `src/scheduler.rs`, `src/event.rs`, `tests/agent_nodes.rs` |
| 4 | `feat(event): approval and revision-request events` | `src/event.rs`, `src/state.rs`, `tests/event_log.rs` |
| 5 | `feat(review): the review inbox as a fold` | `src/review.rs`, `tests/review.rs` |
| 6 | `feat(cli): assembly review` | `src/cli.rs`, `src/main.rs`, `tests/cli.rs` |
| 7 | `feat(scheduler): revise re-seeds a job from its branch` | `src/scheduler.rs`, `src/cli.rs`, `tests/revise.rs` |
| 8 | `feat(delivery): open a PR or push to the base branch` | `src/delivery.rs`, `src/config.rs`, `tests/delivery.rs` |
| 9 | `refactor(gc): scratch worktrees need no policy` | `src/gc.rs`, `src/main.rs`, `tests/cli.rs` |
| 10 | `docs: M3 spec update` | spec, this plan |

## What this deletes from the M2 spec

- *"Worktrees are removed on success and kept on failure."* → always removed.
- *"Approval is part of completion."* → true in supervised mode only.
- *"If a gate would block with no TTY, the run refuses to start."* → it defers.
- **Accepted risk 7** (a retry discards the previous attempt's branch) →
  dissolved. Rounds accumulate as commits on one branch.

## New cost, stated honestly

Branches become the durable artifact, so branch cleanup is now a real concern
where it was not before. `gc` loses ~150 lines of worktree policy and gains a
smaller branch-pruning job. Net simpler — refs are cheap and remote-visible,
directories are neither — but it is not a free deletion.

## Definition of done

- A failed agent node leaves **no** worktree and **one** branch carrying its work.
- A second attempt at a node appends a round to that branch rather than
  replacing it.
- `assembly review` lists merged-but-unreviewed nodes and accepts approve /
  revise.
- `just check` clean; `just verify-stack` clean; test count climbs monotonically.
