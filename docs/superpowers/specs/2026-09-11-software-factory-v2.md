# Software Factory V2 — Design Spec

**Status:** agreed 2026-09-11
**Amended:** 2026-09-21 — F2 design interview. Runner implementors, the wire
envelope, credentials, the job image and `copy` are settled below; see
`docs/superpowers/plans/2026-09-21-software-factory-f2.md` for the decision
table.
**Amended:** 2026-09-28 — F3 design interview. The job boundary moves out to
`assembly run`, `job-exec` and `copy` are deleted, and the daemon's shape —
its root, socket, preflight, queue, reattach and cancel — is settled below;
see the F3 plan for the decision table.
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
| A runner runs `assembly run`, the command a human types | A hidden `job-exec` the host prepares a payload for |
| The daemon launches and watches; the job knows how to be done | A daemon that carries out part of the job |
| The CLI reaches the daemon by HTTP over a Unix socket | A spool directory, or a protocol of our own |
| One runner per daemon | Named runners in a daemon config file |

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

- **No network-facing API until F6.** Polling out, Socket Mode out. No
  ingress, no public URL, no webhook signature verification. The CLI reaches
  the daemon by HTTP over a Unix socket in the daemon's root, which only
  someone on the same machine can open. F6's web UI adds a TCP listener, with
  authentication, in front of the same routes — and that is when a laptop
  can drive a daemon somewhere else.
- **No secrets management.** One credential set, the daemon's, injected into
  jobs as per-job secrets — short-lived once F4 mints them — and destroyed
  on completion. No vault, no rotation, no service accounts.

### Launching and watching

The daemon knows *where* a job runs and *whether* it would be refused. It
does not know how a job is done: that is `assembly run`, and the daemon's
part is to launch it on a runner, collect what it reports, and cancel or
reattach to it.

**One runner per daemon.** `assembly daemon --runner … --max-jobs N` takes
the runner flags `run` used to take — `--runner`, `--image`, `--namespace`,
`--context`, `--pass-env` — and checks the runner once, before it listens; a
runner that fails its preflight stops the daemon starting. A runner is a
*place* jobs launch, not a slot: one k8s runner runs up to `--max-jobs` Jobs
at once. Placing work in two places is two daemons, with two roots. There is
no daemon config file until F4 has sources to declare.

**The root.** `--root`, defaulting to `~/.local/state/assembly-line`, holds
everything the daemon writes. A lock file allows one daemon per root.

```
<root>/daemon.sock  daemon.lock
<root>/repos/<host>/<owner>/<name>.git          bare; claim refs and config reads only
<root>/jobs/<host>/<owner>/<name>/<id>/events.jsonl  job.log  frames
```

**The API** is two routes over the socket: `POST /jobs` submits a new job or
a new round of an existing one, and `POST /jobs/…/cancel` stops one; a health
route lets the CLI say that no daemon is listening. `status` and `logs` read
the root directly, so they work while the daemon is down; `logs -f` is
`tail -f`. Streaming a job's frames over the API waits for F6, which needs it.

**Preflight at submit.** Before anything is claimed, the daemon fetches the
base into its bare cache, pins it to a SHA, validates the config at that SHA
with the same function `run` uses, and checks the job against its runner — a
`--pass-env` name its environment lacks, for one. Every reason is reported at
once and the submit is refused, so a job that would be refused costs no id
and leaves no branch. Only then does it claim `al/job-N` at that SHA (see
*Claiming a job's id*) and queue the job. The pinned SHA travels with the
job, so the config the job reads is the one the daemon validated even if the
base moves while the job waits.

**Queue and cap.** Submitting appends `JobQueued` to the job's own event log.
No more than `--max-jobs` rounds run at once; the rest wait, oldest first. A
job runs one round at a time, so a revise of a job that is queued or running
is refused.

**The fold is the only memory.** On start the daemon folds every job under its
root: a job queued and never launched goes back in the queue, and a round
launched without a verdict is reattached to. Nothing is held that the event
logs cannot rebuild.

**Launch, reattach, cancel.** Launching a round records `RoundLaunched`, with
the round's number — the daemon's fold numbers rounds; `run` does not — and a
handle enough to find the round again:

| Runner | Handle | Reattach | Cancel |
|---|---|---|---|
| local | its `frames` file | read the file from the last `seq` | SIGTERM to its process group |
| docker | container name | `docker logs -f` | `docker stop` |
| k8s | Job name | `kubectl logs -f` | delete the Job |

The local runner starts `run` in a session of its own with stdout on the
`frames` file, so the round outlives the daemon. Docker creates the container
before starting it, so a round cancelled while its image is still pulling
has a container to stop. Frames are deduplicated by `seq` across a reattach.

**Stopping the daemon stops no job.** On SIGTERM it stops accepting work and
exits; its rounds carry on, finishing their arc — pull request included —
without it, and the next start reattaches.

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

### Claiming a job's id

F3 builds the mechanism before there is a source to claim for. A job's id is
taken by creating its branch, `al/job-N`, on the remote at the pinned base
SHA with a push that only creates (`--force-with-lease=refs/heads/al/job-N:`),
one past the highest `al/job-*` the remote lists. A push that loses the race
tries the next id. The daemon claims at submit, so a queued job already has
its id and its branch; `assembly run` with no `--job` claims the same way,
through the same function. Job ids are per repository, as their branches are.

## The runner

One trait, three implementors, all present on day one:

| Implementor | Isolation | Launched by | Event stream |
|---|---|---|---|
| Local process | None | spawning `assembly run`, in a session of its own | the `frames` file its stdout is written to |
| Docker container | Container | the `docker` CLI, `create` then `start` | `docker logs -f` |
| k8s Job | Pod, another machine | the `kubectl` CLI | `kubectl logs -f`, resumed on disconnect |

Three implementors is what earns the trait `CLAUDE.md` forbids defining
speculatively. Each holds differently shaped state while a job runs — a
child, a container name, a Job and its Secret — which is what a trait with a
per-implementor running handle is for.

Docker and k8s drive their CLIs rather than their APIs. Anyone who can use
either runner already has the CLI, auth (kubeconfig, exec plugins, in-cluster
service accounts) comes for free, and the implementors stay testable offline
against shell-script fakes of `docker` and `kubectl`. Moving one implementor
to its API later is invisible above the trait.

**What runs inside the boundary is the whole job, as `assembly run`** — the
command a human types. It claims an id unless given `--job`, clones the job's
branch into scratch, reads and validates the config at the base SHA,
provisions the toolchain when told to, runs the agent, commits, runs
`verify`, pushes, and opens the pull request. The runner launches it with
plain flags — `--repo`, `--ref main@<sha>`, `--job`, `--prompt`,
`--provider`, `--frames`, `--provision-toolchain` — so a round's command line
is its own reproduction: drop `--frames` and type it. Nothing outside the
boundary touches a checkout, so every runner — local included — clones, and
a job can only start from a ref the remote has.

**A revise is `run` on an existing job's branch**, `--job N` with a new
prompt. The agent gets only that prompt; its earlier work is the branch.
Every commit a round makes carries its whole prompt in the commit body, so
what earlier rounds were asked survives on the branch for an agent that
wants to read `git log` — except a round's that changed nothing and so made
no commit.

### Reporting is NDJSON on stdout

A k8s Job shares no filesystem with the daemon, which is what forced this
question. The answer is the one thing all three targets already have: **a
process with stdout and an exit code.**

With `--frames`, `run` emits assembly-line's own event schema — the schema
in `src/event.rs` — inside an envelope, one frame per line:

    {"seq":1,"output":"fake-agent: writing the file"}
    {"seq":2,"event":{"at":"…","t":"round_passed"}}

Without it, `run` prints readable lines for the human at the terminal.

`run` pipes the agent's and `verify`'s output to itself and re-emits
each line as an `output` frame, so nothing an agent *prints* can arrive as
an `event`. That is all it promises: an agent running as `run`'s own
user can still write to `run`'s stdout directly (Accepted risk 11).
Lines that are not frames — `run`'s own stderr, merged in by
k8s — go to the log. `seq` numbers every frame, so a collector that resumes a
dropped stream drops what it already has. A stream that ends without a
verdict gets one from the collector: `RoundFailed` naming why the runner
stopped.

**Config is read at the pinned base SHA, before the agent starts** — by the
daemon, to refuse a job that would fail, and by `run`, to act on it, with the
same validation both times. Never from the job's branch: otherwise a job
could edit the command that verifies it. Everything `run` takes from config
is in memory before the agent runs, so nothing the agent writes changes its
own round's plan.

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

The daemon is the only credential holder, and the holder decides what
leaves. A container job always receives `ASSEMBLY_GIT_TOKEN`, which
`run` wires into a git credential helper for its clone and push, and
`GH_TOKEN`, which `gh` uses to open the pull request. `run` withholds both
from the agent's environment — though an agent running as the same user can
still read them from `run`'s process environment (accepted risk 11). SSH
remotes are rewritten to HTTPS for it.
Anything else the agent needs — its API key — is named by the daemon's
`--pass-env`, and taken from the daemon's own environment. Docker receives values as
`-e NAME`, never on a command line; k8s as a per-job Secret owned by the Job
and deleted with it. The local runner inherits the host's environment, as it
has no isolation to preserve.

**"Per-job" in F2 means scoped to the job's lifetime, not short-lived.**
Minting short-lived git credentials needs a token issuer — realistically a
GitHub App — which is F4 machinery; it is an F4 stretch goal.

Jobs push their own branches, so a pushed branch stays durable even if the
daemon dies mid-job. **A job that cannot push fails:** its only local ref is
in a scratch clone that is deleted with it.

The alternative the old spec named — clone read-only, bundle back, daemon
pushes — removes git credentials from the container but makes durability
depend on the daemon surviving. It is rejected for that reason, not
forgotten.

**Assumption, stated:** jobs run against development repositories with
non-sensitive data. Nothing here is a substitute for secrets management, and
the factory should not be pointed at a repository where it would need to be.

### The job image

One published image, `ghcr.io/hmbill694/assembly-line:<version>`, carrying
`assembly`, `git`, `gh`, `mise`, and the Claude Code, Codex and opencode CLIs. The
runner launches the image whose tag is its own version. A release and its
image are built from the same tag, so a released daemon and the `run` it
launches agree; a build between releases still carries the last version number, and
launches an image that may lag its own code — `--image` points it at one
built from its own tree (`just image`) instead. Users of a release never build
an image. GHCR makes a new package private, so the package is made public once,
by hand, after its first publish; until then no runner can pull it without
credentials.

A repository's toolchain — which `verify` and the agent both need — is
provisioned at job start by `mise install`, from files repositories already
carry: `mise.toml`, `.tool-versions`, `.nvmrc`, `.python-version`,
`rust-toolchain.toml`, `go.mod`. Provisioning runs in containers only; the
local runner uses the host's toolchain. Both container runners provision
cold in F2, so every job pays for its install (see Deferred).
System packages are out of reach until the mise-nix backend lands in F8;
there is deliberately no `setup` field to fill that gap in the meantime.

`copy` is deleted in F3. It seeded gitignored files from a human's checkout,
and no path through the factory has one: a job's settings come from the
committed repository, its secrets from `--pass-env`. A config that still
declares `copy` is refused, with a message saying which of those two to use.

## Per-repo configuration

A repo declares how the factory builds it in `.assembly/config.toml`:

```toml
provider = "claude"
verify = "cargo test"
base = "main"

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

Remediation is a new round of the same job — `assembly run --job N`
submitted with the failure output or the comment as its prompt. The agent's
prior work arrives as files on disk, so it revises rather than restarts,
identically across providers.

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
| `src/gc.rs`, worktrees, `ASSEMBLY_WORKTREE_ROOT` | Jobs clone into scratch that is deleted with them; nothing is left to collect |

Two of the old spec's accepted risks dissolve rather than being fixed: **#5**
(a merge landing in the integration worktree under a running shell node) has
no integration worktree, and **#6** (an agent node merged on exit code alone)
is closed by enforcing `verify`.

F3 deletes a second round, once the boundary moves out to `assembly run`:

| Removed | Because |
|---|---|
| `job-exec`, `ASSEMBLY_JOB`, the payload's round trip through the environment | A runner launches `run` with plain flags |
| `assembly revise`, `revised_prompt` | A revise is `run --job N` with a new prompt |
| `copy`, seeding, `commit_all_except`, the container refusal | Nothing on the factory's path has a checkout to copy from |
| `.assembly/jobs/`, `meta.json`, ids allocated from job directories | State lives in the daemon's root; ids are claimed on the remote |
| `--runner`, `--image`, `--namespace`, `--context`, `--pass-env` on `run` | They configure a daemon; a typed `run` runs where it is typed |

## What survives, promoted

| Kept | New role |
|---|---|
| `src/event.rs`, `src/state.rs` | The NDJSON wire format *and* the daemon's state model — the fold widens from one run to all jobs |
| `revise_node` (`scheduler.rs:320`) | The tester loop's remediation primitive — since F3, `run --job N` |
| `src/git.rs` | Clone, branches, publish — the job's whole mechanism |
| `src/paths.rs` | Where a job's state lives — under the daemon's root since F3 |
| `src/workspace.rs` | A round's scratch clone (its `copy` seeding went in F3) |
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
- **The factory never writes to a user's repository** — neither its working
  tree nor its `.git`. `.assembly/config.toml` is written by humans; all job
  state, and every fetch the daemon makes, lives under the daemon's root.
- Checkouts are scratch and always removed. Branches are the artifact.

## Testing

Unchanged in principle, and the principle is why the NDJSON decision is
affordable.

- Agent execution is tested with **shell-script fakes**, never a real API.
- Concurrency is proven with observable evidence — a wall-clock bound, a probe
  counting live processes — never by inspecting internal state.
- Sources and the forge are tested against **fakes implementing the trait**.
  No test reaches GitHub or Slack.

**No exception.** Every runner, k8s included, is tested against
shell-script fakes of the CLI it drives. Only a real-cluster smoke test
would need a cluster, and none is part of the suite.

## Milestones

| | Scope |
|---|---|
| **F1** ✅ | Subtraction. Delete the DAG, run branch, merge, shell nodes, graph loading, review inbox. `assembly run --repo --ref --prompt` is a single job. `verify` enforced. `.assembly/config.toml`. |
| **F2** ✅ | The runner seam. One trait; local, `docker run`, and k8s Job implementors driving their CLIs. The whole job inside the boundary as `job-exec`; clone, push or fail. NDJSON frames on stdout. Host-resolved payload. Host-chosen per-job credentials. One published image; `mise` provisioning. |
| **F3** | The daemon. `assembly run` is the whole job and what every runner launches; `job-exec`, `revise` and `copy` go. `assembly daemon` with one runner and a concurrency cap; job state, a bare claim cache and an HTTP-over-Unix-socket API under its own root. Preflight and claim-by-ref at submit; queue; fold across all jobs; cancel; reattach for every runner. `submit`, `cancel`; `status` and `logs` read the root. |
| **F4** | Sources and the watcher. Source trait; GitHub issues and Slack. Claim-by-ref, ordering, fairness, dispatch, pull request delivery. Stretch: short-lived, per-job git credentials minted from a GitHub App. |
| **F5** | The tester loop. CI, mergeability and comment signals; budget and refill; `needs-human` escalation; the `auto-merge` path. |
| **F6** | The web UI, served by the daemon. |
| **F7** | The refinement session. |
| **F8** | mise-nix: Nix in the image; system packages declared in a repository's `mise.toml`. Closes the gap F2 leaves open, once the factory itself is finished. |
| *Stretch, after F7* | A toolchain cache, so container jobs stop provisioning cold (Accepted risk 10). See *Deferred* for the constraint any design must meet. |

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
5. **An agent container holds a git credential for the job's lifetime.**
   Mitigated by per-job secrets destroyed with the job — short-lived only
   once F4 mints them — and by the assumption that the factory runs against
   non-sensitive development repositories.
6. ~~Copied files are kept out of commits by accident, not against
   intent, and are not log-safe.~~ Dissolved in F3: `copy` is deleted.
7. **Concurrent jobs on the same repo will conflict**, by design. The tester
   loop absorbs it. If a repo's issues routinely overlap, the loop pays for it
   in rounds, and the answer is fewer concurrent slots for that repo rather
   than a dependency graph.
8. **Two credential-free properties were traded for durability.** Jobs push,
   so they hold credentials. Revisit only if the factory is ever pointed at a
   repository where that is unacceptable — at which point bundle-back is the
   design, and it is written down above.
9. **System packages are unavailable to container jobs** until F8's
   mise-nix backend. A repository that needs one runs on the local runner.
10. **Container jobs provision their toolchain cold**, costing minutes per
    job, until the toolchain cache planned as a stretch goal after F7.
11. **A container job's agent runs as the same user as `run`**, so
    through `/proc` it can read the git and forge tokens from `run`'s process
    environment — or reach them by writing the clone's git config and hooks,
    which the git commands the round runs after the agent then execute with
    the tokens in their environment; only the push is kept from running
    hooks. With the forge token it can open, comment on and close pull
    requests, and merge them once F5's `auto-merge` path grants the token
    that. It can also write to `run`'s stdout via
    `/proc/<pid>/fd/1` and so forge a frame — a verdict included. Isolating
    it needs the agent under its own uid — a later
    milestone's image change. Separately, a docker job's environment — the
    tokens and whatever `--pass-env` named — can be read with `docker inspect`
    by anyone with access to the docker daemon until the container is
    removed at the end of the round.
12. ~~A docker job cancelled while its image is still pulling runs
    anyway.~~ Closed in F3: the container is created before it is started,
    so the daemon's cancel always has one to stop.

## Deferred, knowingly

- Where refinement happens for Slack-sourced work — the thread is the obvious
  home, and it overlaps the web UI's chat. Decided when F7 is reached.
- Web UI authentication.
- Branch pruning. `gc` was deleted in F2 along with worktrees, and nothing
  yet prunes the job branches left on a remote.
- A cache for `mise` provisioning — a docker volume, a PVC, node-local
  storage. Whatever it is, the jobs that read it must not be able to write
  it: a cache the agent can write runs its code in every later job, that
  job's clone and git token included. Provisioning runs the repository's
  own `mise.toml`, which on a revise the agent wrote, so read-only to the
  agent is not enough on its own. A stretch goal after F7; until then
  container jobs provision cold.
- Streaming a job's frames over the daemon's API. F3's `logs -f` tails the
  log file; F6's web UI needs the stream and brings it.
