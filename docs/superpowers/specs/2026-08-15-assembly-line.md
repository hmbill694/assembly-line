# assembly-line — Design Spec

**Status:** agreed 2026-08-15
**Scope:** full product. M1 is the first implementable slice; see Milestones.

## Summary

`assembly` is a Rust CLI that executes a DAG of tasks. Each task is either a
shell command or a coding-agent session. Independent tasks run in parallel.
Agent tasks run in isolated git worktrees and merge back into a run branch.
Supervision is a per-node gate: a human approves or sends the agent back to
revise, or an automated `verify` command approves in their place.

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
`~/.assembly/wt/<run-id>/<node>/`, on a branch off the run branch. The target
repository is never modified and needs no `.gitignore` entry.

Worktrees are removed on success and **kept on failure**, so a failed node can
be inspected. `assembly gc` prunes old ones and runs `git worktree prune`.

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

A supervised node pauses and enqueues an approval. Approvals are served FIFO
on the controlling terminal, one at a time; **other branches keep executing
while you decide.**

```
? impl-auth finished (3 files, +120/-4)
  [a]pprove [r]evise [s]kip [d]iff [x]abort
```

Approving merges the node's branch into the run branch. **Approval is part of
completion** — a node is Done only once approved and merged, so dependents
always branch from reviewed, integrated code. The run branch is always the
approved truth. The cost is that an unattended gate stalls its own downstream
subtree; unrelated branches are unaffected.

### The revise loop

`[r]evise` prompts for free-text feedback and re-invokes the agent **in its
existing worktree**, so it sees its own prior work as files on disk and
revises rather than restarts. This needs no session replay or conversation
history, so it behaves identically across every provider.

The node leaves the approval queue while revising — you are free to handle
other pending approvals — and re-enters it when the round completes. The loop
is **unbounded**: iterate as many times as you like. Human revision rounds do
**not** count against `retries`, which bounds automated verify-failure retries
only.

Every round is recorded (`node_revise_requested` with the feedback text,
`node_started` with a round number), so the log preserves the full
back-and-forth.

### Mode selection

The graph file declares intent; `--unsupervised` forces every gate to
auto-approve and `--supervise-all` forces gates everywhere. If a gate would
block with no TTY and no override, **the run refuses to start** rather than
hanging forever.

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
assembly logs <run-id> <node> [-f]
assembly doctor                  smoke-test each configured provider
assembly gc [--older-than 7d]    prune worktrees and stale run dirs
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
| **M1** | Shell-only parallel DAG: parse, validate, `--jobs`, `resource`, per-node logs, event log, resume-by-replay, skip-subtree failure |
| **M2** | git worktrees, provider invocation, merge into run branch, `copy` seeding |
| **M3** | Supervision gates, revise loop, `verify`, retries, conflict-resolution agent, caps |
| **M4** | Hooks, PR delivery, `status` / `logs` / `doctor` / `init` polish |
| **M5** | Clone, push-per-node, preflight, `gc` |

## Accepted risks

1. `--unsupervised` on an agent node with no `verify` has no safety net and
   cannot auto-resolve conflicts. `validate` warns; it does not error.
2. Failed cloned runs leave branches on origin. `gc` prunes them; follow-on.
3. Shipped default provider configs will go stale as agent CLIs change flags.
   `doctor` detects it, but only if run.
4. Copied secrets are git-safe, not log-safe (see Seeding worktrees).
