# Software Factory V2 — Design Spec

**Status:** agreed 2026-09-11
**Supersedes:** `2026-08-15-assembly-line.md` from M4 onward. M1–M3 shipped and
most of that code survives; what it was *heading toward* does not.
**Scope:** full product. F1 is the first implementable slice; see Milestones.

## Summary

The factory turns an issue into a merged pull request without a human
authoring anything in between.

A daemon watches issue sources, claims work, dispatches one agent job per
issue, and remediates the resulting pull request until it is green. A human
writes the issue and — unless they have said otherwise — presses merge. Nothing
else in the loop needs a person.

```
issue source ──> watcher ──> runner ──> branch ──> PR ──> tester loop ──> merge
   (GitHub,        claim,     local,                        CI, conflicts,
    Slack)         order      docker,                       human comments
                              k8s
```

## What changed, and why

The old spec's unit of work was a **hand-authored DAG of tasks** in a
`graph.toml`. The factory's unit of work is **one issue**. A human who has to
write a task graph before the machine will do anything is doing the job the
machine was built to do.

Everything the DAG existed to express — ordering, exclusivity, fan-out,
partial failure across a subtree — either disappears or moves:

| The DAG expressed | Where it goes |
|---|---|
| One task depends on another | Two issues; the second's PR rebases |
| Two tasks conflict in the tree | The tester loop, as a non-mergeable PR |
| Fan-out across a graph | Concurrent jobs across issues |
| A subtree is skipped on failure | A job fails alone; nothing is downstream of it |

The **job contract is unchanged and is now the only contract**:

> A node's branch always survives; a node's worktree never does.
> A job is `(repo, ref, prompt) -> branch`.

M3 was built to make that sentence true. This spec is what it was for.

## Decided, with the rejected alternative

These were resolved deliberately. Reopening one is a design change, not a
clarification.

| Decision | Rejected |
|---|---|
| One issue = one job | A planner agent emitting a DAG the scheduler runs |
| Conflicts are the tester loop's input | A dependency graph derived from the tracker |
| No control-plane service; NDJSON on stdout | Runners POSTing to an API |
| No database; state is a fold | SQLite as a source of truth |
| Issue source owns draft state | The factory storing drafts |
| Terminal state is a green PR a human merges | Lights-out auto-merge for everything |
| `verify` in-job **and** forge CI | Either one alone |
| Refinement is not a job | Widening the job contract to carry conversation |
| Daemon is the only credential holder | Per-environment secret provisioning |
| Delete-first, in place | A second crate, or an additive rewrite |

## The daemon

One deployable. It runs the watcher loop, the tester loop, and the web UI, and
it holds every credential in the system.

**State is a fold, as it always was.** `RunState` widens from one run to all
jobs: the daemon reads each job's event stream and derives what is in flight,
what is waiting, and what failed. There is **no database**. A store may later
be added as a *cache* over the same events; it never becomes the truth.

Everything mutable that a human touches — drafts, priority, claims, status —
lives in the issue source, which already has an editor, a history and an
audit trail. The factory stores none of it.

### What the daemon does not have

- **No inbound API.** Polling out, Socket Mode out. No ingress, no public URL,
  no webhook signature verification.
- **No secrets management.** One credential set, the daemon's, injected into
  jobs as short-lived per-job secrets and destroyed on completion. No vault,
  no rotation, no service accounts.

## Issue sources

A source is a trait with two implementors on day one, so it is earned rather
than speculative. Its vocabulary is the **factory's**, not a forge's —
`work_ready_to_claim`, `claim`, `report_status`, `mark_delivered`. A trait
that reads like GitHub's API with a `dyn` in front of it is not pluggable.

Every source must supply: a stable id, a body that becomes the prompt, a
*ready* signal, a *priority* flag, an *auto-merge* flag, and a channel to
report status back on.

### GitHub issues

| Concept | Mechanism |
|---|---|
| Draft | `draft` label; excluded from the watcher's query |
| Ready | Absence of `draft` |
| Priority | `priority` label |
| Auto-merge permitted | `auto-merge` label, carried into the job payload |
| Claimed | `in-progress` label (visibility) + the claim ref (correctness) |
| Status | Issue comments and the linked PR |
| Done | Issue closed by the merged PR |

### Slack

Slack is a **peer source**, not an intake funnel: the watcher polls it
directly and a Slack-sourced job needs no tracker issue at all. Reactions are
the label vocabulary.

| Concept | Mechanism |
|---|---|
| Draft | The refinement thread, before it is marked ready |
| Ready | ✅ reaction |
| Priority | 🔥 reaction |
| Claimed | 👀 reaction (visibility) + the claim ref (correctness) |
| Status | Replies in the thread |
| Done | A reaction applied by the bot on merge |
| Repo | Channel → repo mapping in daemon config |

A Slack message names no repository, so the binding is a deployment fact and
lives in daemon config. Where a channel maps to more than one repo, the bot
asks in-thread and a reaction picks.

**Stated plainly:** Slack's message retention becomes the durability of that
queue. Work that scrolls out of retention is gone. Teams that need a durable
backlog should use a tracker.

## The watcher loop

Polls each configured source on an interval. No webhooks — a daemon that
cannot be reached is a deployment constraint, and polling survives it.

**Ordering.** FIFO by creation, with a `priority` flag that jumps the queue.
Priority is scoped *within* a repo, so the flag means one thing and needs no
cross-repo arbitration.

**Fairness.** A single global concurrency cap bounds total load and spend —
the one number worth tuning when agents bill per token. The watcher takes the
next eligible item from each repo in turn, so a repo that files fifty issues
at once cannot starve the others.

### Claiming

Two agents on one issue is the expensive failure: duplicate PRs, duplicate
spend, conflicting branches. A daemon holding claims in memory re-dispatches
everything after a restart, and two daemons double-dispatch always.

**The claim is a git ref.** Before dispatch the watcher pushes
`al/<source>-<id>` to the remote. Ref creation is atomic — the second
watcher's push is rejected and it moves on. Then it marks the source, so
humans can see what the factory took.

Correctness comes from git; visibility comes from the source. The claim ref is
also the job's base, so the claim and the work are the same object.

## The runner

One trait, three implementors, all present on day one:

| Implementor | Isolation | Event stream |
|---|---|---|
| Local process | None | A pipe |
| `docker run` | Container | `docker logs` |
| k8s Job | Pod, another machine | The k8s log API |

Three implementors is what earns the trait `CLAUDE.md` forbids defining
speculatively.

### Reporting is NDJSON on stdout

A k8s Job shares no filesystem with the daemon, which is what forced this
question. The answer is the one thing all three targets already have: **a
process with stdout and an exit code.**

The runner emits assembly-line's own event schema — the schema in
`src/event.rs`, unchanged — as newline-delimited JSON on stdout, and pushes
its branch. The daemon collects that stream three ways behind the trait.

This is the *adapter* contract the old spec already described, promoted from
optional enrichment to the reporting path. It means:

- No inbound API, no auth, no credential handed to an agent for the daemon
- One reporting mechanism for every target
- The whole loop is testable with the existing shell-script fakes, offline

**The cost:** an event is durable only once the daemon has collected it. A pod
garbage-collected before collection loses its stream. The *branch* survives
regardless, which is the property that matters and the reason the job contract
is shaped the way it is.

### Credentials

The daemon is the only credential holder. Each job receives an ephemeral,
short-lived secret — a git credential and the agent's API key — destroyed on
completion. Jobs push their own branches, so a pushed branch stays durable
even if the daemon dies mid-job.

The alternative the old spec named — clone read-only, bundle back, daemon
pushes — removes git credentials from the container but makes durability
depend on the daemon surviving. It is rejected for that reason, not
forgotten.

**Assumption, stated:** jobs run against development repositories with
non-sensitive data. Nothing here is a substitute for secrets management, and
the factory should not be pointed at a repository where it would need to be.

## Per-repo configuration

A repo declares how the factory builds it in `.assembly/config.toml`:

```toml
provider = "claude"
verify = "cargo test"
base = "main"
copy = [".claude/settings.local.json"]

[merge]
auto = false          # the `auto-merge` label may still permit it per issue
```

Versioned, PR-reviewable, travels with the code, and already present in the
job's own checkout. A repo with no file is simply not opted in.

**Read it from the base ref, never the agent's branch.** Otherwise a job can
edit the command that verifies it — harmless behind a human merge gate, and
not harmless at all behind the `auto-merge` label.

**Naming.** `.assembly/` in a repository now means *configuration* and is
tracked. All job state moves under the daemon's own root. The two meanings
never share a directory.

## Verification

Two signals, with different jobs:

- **`verify`, in-job, after the commit.** A fast local filter so the factory
  never *delivers* a pull request on code that does not compile, and the
  signal the tester loop iterates against without paying for a CI cycle per
  round. The branch is published either way — a rejected job is exactly the
  case where the diff is worth reading.
- **Forge CI, as the merge gate.** It is what branch protection actually
  enforces, so it is what "green" has to mean.

This makes `verify` load-bearing for the first time. It has been parsed and
ignored since M1 — the old spec's accepted risk #6 — and enforcing it is a
**behavior change**, not a cleanup.

## The tester loop

Reads three signals off an open pull request:

| Signal | Trigger |
|---|---|
| Failing checks | Machine — remediation job with the failure output as feedback |
| Not mergeable | Machine — remediation job that rebases and resolves |
| Human comment | Human — remediation job with the comment as feedback |

Remediation is `revise_node` (`src/scheduler.rs:320`) unchanged: free-text
feedback, re-seed a job from the branch tip, append a round. The agent's prior
work arrives as files on disk, so it revises rather than restarts, identically
across providers.

There is **no agent reviewer** on day one. It is the piece most likely to
produce confident noise, and adding it later costs nothing — "a job that posts
a pull request comment" needs no new plumbing.

### The budget

Carried forward from the old spec, which already drew this line correctly:

> The loop is unbounded. Human revision rounds do **not** count against
> `retries`, which bounds automated re-invocations only.

Machine-triggered rounds decrement the budget. A human comment is unbounded
and **refills** it — a human spending a round is a decision, not a spin.

At zero: label the pull request `needs-human`, comment naming the last
failure, emit the event. The branch and the pull request survive untouched.
Nothing is closed and nothing is discarded, per the job contract.

### Terminal state

The factory's product is a mergeable pull request: green, no unresolved
threads, no conflicts. A human merges it.

An issue carrying the `auto-merge` label produces a job that merges itself
when green. The label travels from the source into the job payload and onto
the pull request, so the decision is visible at every step and is made by
whoever wrote the issue.

Two paths, deliberately. The default cannot break trunk. The opt-in path lets
you dial trust per issue class — dependency bumps merge themselves, features
do not.

## Refinement

An interactive agent session that turns a half-formed request into a
well-formed issue, and posts it for human review before the watcher can see
it.

**It is not a job.** Refinement is multi-turn, streaming, human-in-the-loop,
and produces prose. The job contract is deliberately shaped so that nothing is
kept alive between rounds and no session replay is needed. Pushing
conversation through it would undo exactly the property M3 was built to
establish.

So: two agent-invocation paths, because there are genuinely two shapes —
headless batch through the runner, and one interactive session in the daemon.
The refinement session gets a read-only checkout for codebase context and
write access to the source. Nothing more.

The human gate is on the **input**, symmetric with the merge gate on the
output. The factory never acts on a requirement nobody approved.

## The web UI

Served by the daemon, which already holds the folded state and already
collects the event streams. HTTP plus a server-sent event stream over what
exists; one deployable with embedded assets.

It shows what is in flight, tails a job's output live, and offers approve and
revise as actions. It is the observability surface — the thing a team watches
— not a second place the truth lives.

## What this deletes

| Removed | Because |
|---|---|
| `src/dag.rs` — edges, cycles, `descendants` | No topology to validate |
| `needs`, `resource`, `on_failure`, `NodeSkipped`, `mark_pending_as_skipped` | All express relationships inside one run |
| Run branch, integration worktree, `prepare_run_branch`, `merge`, `MergeOutcome`, `NodeMergeConflicted` | A job branches from base and opens a PR against it |
| `kind = "shell"`, `TaskKind` | No user-authored shell nodes; `verify` is a field |
| `parse_graph`, `load_graph`, `inline_prompt_files`, `prompt_file` | The prompt comes from an issue |
| `src/review.rs`, `assembly review` | The pull request is the inbox |
| `supervise`, `Supervise` | Review moved to the pull request |
| `hooks`, `output_file`, `{{ tasks.<id>.output }}` | Parsed and referenced nowhere; the last needed siblings |
| `delivery.mode = "push"` | Auto-merge-by-label replaces it |

Two of the old spec's accepted risks dissolve rather than being fixed: **#5**
(a merge landing in the integration worktree under a running shell node) has
no integration worktree, and **#6** (an agent node merged on exit code alone)
is closed by enforcing `verify`.

## What survives, promoted

| Kept | New role |
|---|---|
| `src/event.rs`, `src/state.rs` | The NDJSON wire format *and* the daemon's state model — the fold widens from one run to all jobs |
| `revise_node` (`scheduler.rs:320`) | The tester loop's remediation primitive, unchanged |
| `src/git.rs` | Worktrees, branches, publish — the job's whole mechanism |
| `src/paths.rs` | Already keyed by repo slug, so already multi-repo shaped |
| `src/workspace.rs` | `copy` seeding, with `.git/info/exclude` keeping secrets off branches |
| `src/provider.rs`, `src/exec.rs` | Vendor-neutral agent invocation, unchanged |
| `src/delivery.rs` | Folds into the forge trait's `open_change` |
| `verify`, `retries`, `max_duration` | Enforced for the first time |
| `src/config.rs` | Shrinks to per-repo config; does not die |

Roughly a third of `src/`'s ~4,400 lines go, and a comparable share of tests.
The remainder is close to exactly right, which is the argument for deleting in
place rather than starting over.

## Invariants

Carried forward, with two amendments.

- The event log is **append-only**. Never rewritten or truncated.
- State is a **pure fold** over the event stream. Anything that cannot be
  reconstructed from events does not belong in it — now across all jobs, not
  one run.
- Ids match `^[A-Za-z0-9_-]+$` — they become filenames and branch names.
  Validate, never sanitize. Source ids are normalized into this shape, never
  trusted raw.
- **The factory never writes to a target repository's working tree.**
  `.assembly/config.toml` is written by humans; all job state lives under the
  daemon's root.
- Worktrees are scratch and always removed. Branches are the artifact.

## Testing

Unchanged in principle, and the principle is why the NDJSON decision is
affordable.

- Agent execution is tested with **shell-script fakes**, never a real API.
- Concurrency is proven with observable evidence — a wall-clock bound, a probe
  counting live processes — never by inspecting internal state.
- Sources and the forge are tested against **fakes implementing the trait**.
  No test reaches GitHub or Slack.

**The one exception to record:** the k8s runner cannot be tested without a
cluster or a fake API server. That implementor, and only that implementor, is
feature-gated out of the default suite — the same treatment the old spec gave
real-agent smoke tests.

## Milestones

| | Scope |
|---|---|
| **F1** | Subtraction. Delete the DAG, run branch, merge, shell nodes, graph loading, review inbox. `assembly run --repo --ref --prompt` is a single job. `verify` enforced. `.assembly/config.toml`. |
| **F2** | The runner seam. One trait; local, `docker run`, and k8s Job implementors. NDJSON on stdout. Daemon-injected per-job credentials. |
| **F3** | The daemon. Long-lived process, fold across all jobs, job state under its own root, CLI driving it. |
| **F4** | Sources and the watcher. Source trait; GitHub issues and Slack. Claim-by-ref, ordering, fairness, dispatch, pull request delivery. |
| **F5** | The tester loop. CI, mergeability and comment signals; budget and refill; `needs-human` escalation; the `auto-merge` path. |
| **F6** | The web UI, served by the daemon. |
| **F7** | The refinement session. |

F1 is pure subtraction and is where the stack starts. Enforcing `verify` is a
behavior change and gets its own change in that stack rather than riding along
with a deletion.

## Accepted risks

1. **A subtraction milestone makes the tool do less than it does today** for
   the length of F1. Accepted as the cost of not maintaining two engines.
2. **Slack retention bounds the durability of a Slack-sourced queue.** Work
   that scrolls out is gone. Documented, not mitigated.
3. **NDJSON durability depends on collection.** A pod GC'd before its stream
   is read loses its events. The branch survives, which is the property the
   job contract guarantees.
4. **The `auto-merge` path has the trunk as its blast radius.** Its only gate
   is forge CI. Reading config from the base ref closes the
   self-modification hole; a weak test suite is not closed by anything here.
5. **An agent container briefly holds a git credential.** Mitigated by
   short-lived per-job secrets and by the assumption that the factory runs
   against non-sensitive development repositories.
6. **Copied files are git-safe, not log-safe.** Carried over unchanged:
   nothing prevents an agent from printing a copied file's contents to stdout,
   which now lands in a stream the daemon collects.
7. **Concurrent jobs on the same repo will conflict**, by design. The tester
   loop absorbs it. If a repo's issues routinely overlap, the loop pays for it
   in rounds, and the answer is fewer concurrent slots for that repo rather
   than a dependency graph.
8. **Two credential-free properties were traded for durability.** Jobs push,
   so they hold credentials. Revisit only if the factory is ever pointed at a
   repository where that is unacceptable — at which point bundle-back is the
   design, and it is written down above.

## Deferred, knowingly

- Where refinement happens for Slack-sourced work — the thread is the obvious
  home, and it overlaps the web UI's chat. Decided when F7 is reached.
- Web UI authentication.
- Container image strategy for the Docker and k8s runners.
- How `copy`-seeded files reach a remote runner without landing in a branch or
  a log.
- Branch pruning across many repositories. `gc` lost its worktree policy in M3
  and has not yet gained the branch job that replaces it.
