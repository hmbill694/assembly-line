# assembly-line — Design Spec

**Status:** agreed 2026-08-15
**Scope:** full product. M1 is the first implementable slice; see Milestones.

## Summary

`assembly` is a Rust CLI that executes a DAG of tasks. Each task is either a
shell command or a coding-agent session. Independent tasks run in parallel.
Agent tasks run in isolated git worktrees and merge back into a run branch.
Supervision is a per-node gate: a human approves or sends the agent back to
revise, or an automated `verify` command approves in their place.

## The job contract

**A node's branch always survives; a node's worktree never does.**

A job is stateless. It takes a repository, a base ref and a prompt, and
produces a branch — pushed when there is a remote, left as a local ref when
there is not. Its checkout is scratch and is discarded whatever happened,
including on failure.

Everything else follows from that sentence:

| Consequence | Why it matters |
|---|---|
| Failure is a branch, not a directory | Inspectable from any machine, not only the one that ran it |
| Revising re-seeds from the branch | Nothing has to be kept alive on disk between rounds |
| `(repo, ref, prompt) -> branch` | Already the shape of a `docker run` or a k8s Job |

## Core model

A TOML graph file declares tasks. `needs` forms the edges.

```toml
[[task]]
id = "build"
kind = "shell"
run = "cargo build"

[[task]]
id = "impl-auth"
kind = "agent"
needs = ["build"]
prompt = "Implement JWT auth per spec"
verify = "cargo test -p auth"
supervise = "on-complete"
```

Ready nodes execute in parallel up to `--jobs` (default 4). Nodes sharing a
`resource = "..."` label never run concurrently — this expresses exclusivity
the DAG cannot (a fixed port, a shared database, a single GPU).

## Agent execution is vendor-neutral

A provider is `cmd` + `args`, with `{prompt}` interpolated. No vendor is
special-cased.

```toml
[providers.claude]
cmd = "claude"
args = ["-p", "{prompt}", "--permission-mode", "acceptEdits"]

[providers.codex]
cmd = "codex"
args = ["exec", "{prompt}"]

[providers.aider]
cmd = "aider"
args = ["--message", "{prompt}", "--yes"]
```

A task's prompt may be inline or in a file:

```toml
[[task]]
id = "impl-auth"
kind = "agent"
prompt_file = "prompts/auth.md"   # mutually exclusive with `prompt`
```

`prompt_file` resolves **relative to the graph file**, so a graph and its
prompts move together as one unit. The file is read at load time and folded
into `prompt`, so nothing downstream knows which form was used, and a missing
or unreadable prompt file fails before a run directory is allocated.

**The engine requires only three things from an agent process:** its exit
code, the git diff it leaves in the worktree, and the result of `verify`. Git
is the ground truth — the engine parses no vendor output format, so no
vendor's format change can break it.

A provider may optionally point at an *adapter* — a wrapper script emitting
NDJSON in assembly-line's own schema — to supply cost, turn count, or a
session id. Adapters are enrichment: if one breaks, you lose a readout, not
the run.

Provider variants are separate config blocks (`claude`, `claude-yolo`) rather
than an engine-modelled permission vocabulary. `args` bake in whatever flags
make that agent run headless.

## Isolation: three separable questions

Where the *run* executes, where each *agent* executes, and who owns the
workspace are independent decisions. Conflating them pushes complexity into
the engine that belongs in deployment.

| Concern | Answer | When |
|---|---|---|
| Agents must not touch the host machine | Containerize the **whole run** — a Dockerfile, no engine code | now |
| Nodes need different images or resource caps | A containerized agent is **just a provider** (`cmd = "docker"`) | when needed |
| Agents must not affect each other mid-run | A real `AgentRunner` seam: runner-owned workspaces, work returned as a git bundle | deferred |

The third is the only one that requires an abstraction in Rust, and it is
**deliberately deferred** until a concrete use case exists — consistent with
not defining a trait before a second implementor does.

Note for whoever picks that up: a bind-mounted worktree is *not* a usable
shortcut. A worktree's `.git` is a file holding an absolute path to
`<repo>/.git/worktrees/<name>`, so making git work inside a container means
mounting the real object store read-write — handing the container the ability
to rewrite any ref. The isolating design is a read-only repo mount, a clone
inside, and a bundle back.

## Isolation between nodes

Each agent node runs in its own `git worktree` at
`~/.assembly/wt/<repo-slug>/<run-id>/<node>/`, on a branch off the run branch.
The target repository is never modified and needs no `.gitignore` entry.

Run ids restart at 1 in every repository, so the path is keyed by repository as
well as run: `<repo-slug>` is the repository's own directory name plus a stable
hash of its absolute path, and a `repo` marker file beside it records the full
path so `gc` can collect a repository's leftovers without being run from it.
`$ASSEMBLY_WORKTREE_ROOT` moves the whole tree off `$HOME`.

The run branch is also checked out into a worktree of its own — the
*integration worktree* — so merges never touch the branch you have checked out.

**Branch naming.** Node branches are siblings of the run branch, not children:

```
al/run-42               run branch (integration worktree)
al/run-42-impl-auth     node branches
al/run-42-impl-api
```

Git refs are paths, and a ref cannot be both a file and a directory — so
`al/run-42` and `al/run-42/impl-auth` **cannot coexist**. The flat form still
groups under `al/run-42*` for cleanup.

Worktrees are **always removed**, success or failure — they are scratch. What
a failed node leaves is its *branch*, carrying whatever the agent managed
before it gave up, published but never merged. `assembly gc` collects the
integration worktree once a run's state directory is gone, plus anything a run
that died mid-node orphaned.

### Seeding worktrees

A worktree contains only tracked files. Untracked local config an agent needs
(`.env`, `.claude/settings.local.json`) is declared explicitly:

```toml
[workspace]
copy = [".env", ".claude/settings.local.json"]

[[task]]
id = "e2e"
copy = ["fixtures/seed.sql"]   # appends to the workspace list
```

Paths resolve on the machine running the CLI, so cloned runs behave
identically. Each copied path is written to the worktree's
`.git/info/exclude`, so an agent cannot commit a secret onto a branch bound
for a remote. A declared path that does not exist is a preflight error.

**Known limit:** copied files are git-safe but not log-safe. Nothing prevents
an agent from printing `.env` contents to stdout, which lands in the node log.

## Supervision

`supervise = "none" | "pre" | "on-complete" | "both"` per node.

Review is a **second axis, orthogonal to execution**. A node is `Done` because
it ran; whether anyone has looked at it is tracked separately:

```
execution:  Pending | Running | Done | Failed | Skipped
review:     NotGated | Unreviewed | Approved | RevisionRequested
```

The graph declares the *intent*; the invocation decides the *timing*. A gate
either blocks now or becomes a review item later:

| | Supervised | Unsupervised |
|---|---|---|
| Node hits its gate | Blocks, prompts on the TTY | Merges on `verify`, files a review item |
| Dependents branch from | **Reviewed** code | **Verified** code |
| A run can stall | Yes, its own subtree only | Never |

`--unsupervised` therefore means **defer**, not ignore — which is why there is
no need for the earlier rule that a run refuses to start without a TTY. It
downgrades instead.

**The cost, stated plainly:** under deferral, dependents branch from verified
rather than reviewed code. Rejecting at 9am work that merged at 2am is not a
rewind — the revision lands *on top* of whatever followed. `review` reports the
blast radius so an approval is informed rather than blind.

### The review inbox

```
run 42: 2 awaiting review

  ? impl-auth  3 files +120/-4   al/run-42-impl-auth
  ? impl-api   1 file  +12/-0    al/run-42-impl-api

assembly review 42 --approve <node>
assembly review 42 --revise <node> "what to change"
```

Derived entirely from the event log, not from the graph's `supervise` field —
the graph file may have changed since the run, and the log is what actually
happened. Verdicts are appended to that same log, which is append-only, so a
verdict is simply another event.

### The revise loop

`assembly revise <run> <node>` starts a **new job** based at the node's branch
tip. The agent's prior work arrives as files on disk, so it revises rather than
restarts — needing no session replay or conversation history, and behaving
identically across every provider.

Crucially this needs **nothing kept alive between rounds**. The round checks
the branch back out into fresh scratch, appends its commit to that branch, and
discards the checkout again. Its diff is measured against the previous round,
which is what a reviewer wants to see.

The loop is **unbounded**. Human revision rounds do **not** count against
`retries`, which bounds automated verify-failure retries only. Every round is
recorded (`node_revision_requested` with the feedback text, `node_started` with
a round number), so the log preserves the full back-and-forth.

## Unsupervised execution

Gates auto-approve, with `verify` standing in for the human. Pass → merge.
Fail → retry with the failure output fed back as notes — mechanically the same
revise loop, with the verify output in place of human feedback — up to
`retries`. Then the node fails.

Caps, all per-node:

- `max_duration` — wall clock, enforced by killing the process. Universal.
- `retries` — bounded automated re-invocations.
- `max_cost_usd` — honored **only** when the provider has an adapter that
  reports cost. `validate` warns when it is set on a provider that cannot
  supply one, so it never silently does nothing.

A run-wide `max_duration` bounds the whole graph.

## Merge conflicts

Two kinds, handled differently:

- **Run branch vs. base**, at the end: an ordinary PR conflict. GitHub's
  problem. The engine does nothing.
- **Node branch vs. run branch**, mid-run: GitHub never sees this, and it
  stalls the subtree. On conflict, a resolution agent runs in the node's
  worktree with the conflict markers in context; **its result must pass the
  node's `verify` before merging.** A node with no `verify` gets no autonomous
  resolution and fails. Supervised runs surface the conflict at the gate with
  a resolve-with-agent option.

## Failure semantics

A node that exhausts its retries fails. Its descendants are marked skipped;
independent branches run to completion. The run ends `partial` with a nonzero
exit code. `on_failure = "skip" | "abort" | "continue"` overrides per node,
defaulting to `skip`.

Whatever the agent produced before failing is committed and published anyway —
a half-finished failure is exactly the case where the diff is worth reading —
but it is never merged into the run branch.

## Delivery

```toml
[delivery]
mode = "pr"      # or "push", or "none"
base = "main"    # defaults to the branch the run started from
```

`pr` pushes the run branch and opens a pull request. `push` fast-forwards the
base on the remote, for work trusted to land unreviewed; it is deliberately
not forced, so a base that moved underneath the run is a reported conflict
rather than a silently overwritten commit.

The base is never an assumed `main` — a run does not touch the repository's own
HEAD, so it still says what branch you were standing on.

`gh` is not a dependency. If it is missing or refuses, the branch is already on
the remote and that is reported; a human can open the pull request themselves.
Delivery runs only when the whole graph succeeded: a partial run still leaves a
real branch, but opening a pull request for unfinished work is noise, so the
branch name is printed instead.

## Context flow

Primary channel is the merged code itself — a dependent branches from a run
branch that already contains its upstream's work.

On top, prompts may interpolate:

- `{{ tasks.<id>.output }}` — the node's declared `output_file` read from its
  worktree, or the last 100 lines of stdout if none is declared.
- `{{ tasks.<id>.diff }}` — derived from git, requiring no agent cooperation.

`output_file` is vendor-neutral: you get it by telling the agent in its prompt
to write the file.

## State and resume

State is an append-only `events.jsonl`. In-memory state is a fold over that
log; resume is a replay. Append is atomic, so a crash cannot corrupt it, and
the log doubles as the audit trail and cost ledger.

- **Local runs:** `<git-root>/.assembly/runs/<id>/`
- **Cloned runs:** `~/.assembly/runs/<id>/`, written from the first event so
  teardown of the ephemeral checkout cannot lose them.

`meta.json` records repo, ref, base sha, and run branch — everything `resume`
needs.

## Clone support

```
assembly run <graph> --repo <url> [--ref <branch|tag|sha>]
```

Preflight runs before any network cost: `git ls-remote`, graph parse and
validation, `copy` path existence. A typo fails in milliseconds instead of
after a two-minute clone.

Full clone into a tempdir, deleted on exit. `--depth` is opt-in and warns that
`diff base...branch` degrades. The CLI never handles credentials — it inherits
ssh-agent, the git credential helper, or `GH_TOKEN`.

**Durability comes from the remote.** Every approved-and-merged node
immediately pushes the run branch. Teardown is then safe by construction, a
partial run leaves a real inspectable branch on origin, and `resume`
re-clones that branch and continues from the event log. Local runs keep the
branch on disk and push once at the end.

Graph is a local path by default (validated before cloning); `--graph-in-repo
<path>` reads it from the clone instead.

## Hooks

Hooks key off the same event vocabulary as the log and receive the event as
JSON on stdin. A nonzero exit is reported but never fails the run.

```toml
[[hook]]
on = "run_complete"
when = "success"
run = """git push -u origin $AL_BRANCH &&
  gh pr create --base $AL_BASE --head $AL_BRANCH --fill"""

[[hook]]
on = "node_failed"
run = "notify-send \"$AL_NODE failed\""
```

The default `run_complete` hook opens a PR on success. Base is the branch you
started from (local runs) or `--ref` (cloned runs) — never an assumed `main`.

## Commands

```
assembly init                    scaffold graph.toml
assembly validate <graph>        cycles, unknown deps, providers, copy paths
assembly run <graph> [--jobs N] [--unsupervised] [--supervise-all]
                     [--repo URL] [--ref REF] [--graph-in-repo PATH]
assembly resume <run-id>
assembly status [run-id]         node tree, timings, costs
assembly review [run-id] [--approve NODE] [--revise NODE FEEDBACK]
                                 gates a run deferred, and verdicts on them
assembly revise <run-id> <node> [feedback]
                                 another round, based on the node's branch
assembly logs <run-id> <node> [-f]
assembly doctor                  smoke-test each configured provider
assembly gc [--older-than 7d] [--dry-run]
                                 prune worktrees left by failed nodes
```

## Stack

tokio (+ tokio-util for cancellation), clap, serde / toml / serde_json, git
via subprocess, anyhow + thiserror, tracing.

## Testing

Engine logic — scheduler, gates, worktrees, merges, conflicts, resume — is
tested with **shell-script fake agents** that edit files and exit with chosen
codes. Fast, free, deterministic, and involving no vendor formats, because the
minimal contract means there are none to involve.

Real-agent verification is a separate, feature-gated smoke suite: one trivial
task ("create hello.txt") per configured provider. `assembly doctor` runs the
same check on demand. This is the only guard against a shipped provider config
going stale as agent CLIs change their flags.

## Milestones

| | Scope |
|---|---|
| **M1** ✅ | Shell-only parallel DAG: parse, validate, `--jobs`, `resource`, per-node logs, event log, resume-by-replay, skip-subtree failure |
| **M2** ✅ | git worktrees, provider invocation, merge into run branch, `copy` seeding |
| **M3** ✅ | Stateless jobs: branch survives / worktree does not, publish on failure, review inbox, revise-as-new-job, PR and push delivery |
| **M4** | The runner seam: local and container implementors, then k8s Jobs |
| **M5** | Detached runs, `assembly runs`, PTY `attach`, blocking supervised gates |
| **M6** | Built-in refinement prompt and plan schema; `verify`, retries, conflict-resolution agent, caps |
| **M7** | Clone, preflight, `doctor` / `init`, `gc` for remote branches |

**Deferred from M3, deliberately.** `verify` and `retries` are parsed but not
yet enforced, so the only thing gating a merge today is the agent's exit code.
Blocking supervised gates need the PTY work in M5, so M3 ships the deferred
half of supervision only. Neither is an oversight; both are sequencing.

## Accepted risks

1. `--unsupervised` on an agent node with no `verify` has no safety net and
   cannot auto-resolve conflicts. `validate` warns; it does not error.
2. Failed cloned runs leave branches on origin. `gc` prunes them; follow-on.
3. Shipped default provider configs will go stale as agent CLIs change flags.
   `doctor` detects it, but only if run.
4. Copied secrets are git-safe, not log-safe (see Seeding worktrees).
5. A merge into the run branch lands in the integration worktree while an
   unrelated shell node may be running there, so that node can observe the tree
   changing under it. Merges are serialized against each other, but not against
   shell nodes. Closing this needs the runner seam in M4, where a shell node
   gets its own checkout too.
6. An agent node with no gate has nothing checking its output: it is committed
   and merged on exit zero alone, because `verify` is parsed but not yet
   enforced. `validate` warns (risk 1); M6 is what actually closes it. Under
   `--unsupervised` this warning should become an error, and does not yet.
7. Branches are now the durable artifact, so branch cleanup is a real concern
   where it was not before. `gc` lost ~150 lines of worktree policy and has not
   yet gained the branch-pruning job that replaces it. Refs are cheap, so this
   is untidy rather than urgent.
8. A revise round appends to the node's branch and merges on top of whatever
   landed after it. That is forward-fixing, never a rewind — correct, but it
   means a rejected node's *original* work stays in the run branch's history.
