# Software Factory F2 Implementation Plan — the runner seam

**Status:** planned 2026-09-21; implemented. The code and its commit messages
are the record now — where they and this plan disagree, they win. Known
places this plan is stale, so that nobody copies them back in:

- Rust is pinned to 1.98.1, not 1.97.1, in `rust-toolchain.toml` and the
  Dockerfile's builder image.
- The pinned Codex download URL below
  (`releases/${CODEX_VERSION}/download/...`) 404s; the Dockerfile uses
  `releases/download/${CODEX_VERSION}/...`.
- The k8s runner creates its Job and Secret with `kubectl create`, never
  `apply` — `apply` copies the Secret's values into an annotation.
- Docker cancels with `docker stop`, not `rm -f`, so `job-exec` can report
  the round; the container is removed afterwards.
- `job_plan`, `inspect_oom` and `logs_of` do not exist under those names.

Stale in ways that would reopen a hole if copied back:

- `JobSecrets` does not derive `Debug` — its `Debug` shows names only — and
  refuses the names assembly-line reserves.
- `Runner::launch` takes a `CancellationToken`, so cancelling reaches a
  runner that is still launching.
- The job's push runs with `-c core.hooksPath=/dev/null`, from `HEAD`
  (`git::push_head_as`), not `push_branch`.
- There is no `assembly-mise-cache` volume: a cache every job could write
  would run one job's code in the next. Container rounds provision cold.
- A clone commits as assembly-line (`git::commit_as_assembly_line`), never
  as whatever identity the environment has.
- The k8s preflight asks about every permission a round uses, not only
  `create`, and a job's pod gets no service-account token.
- `branch_exists` and `remote_exists` are gone.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A job runs behind one `Runner` seam — as a local child process, a
`docker run` container, or a k8s Job — reports itself as numbered NDJSON
frames on stdout, and pushes its own branch; the host collects the stream into
the same `events.jsonl` and log it writes today.

**Architecture:** The whole job moves inside the boundary as a hidden
`assembly job-exec`, which clones the remote into a scratch directory, runs
the agent and `verify`, pushes or fails, and prints frames. The host resolves
everything the job needs into a SHA-pinned `JobPayload` *before* launching it
and hands it over in `ASSEMBLY_JOB`. Runners differ only in how they launch
`job-exec`, stream its stdout, report why it died, and tear it down; the
collector above them is runner-agnostic. Docker and k8s drive the `docker`
and `kubectl` CLIs and are tested offline against shell-script fakes of those
CLIs. One published image carries `assembly`, `git`, `mise` and the agent
CLIs; a repository's toolchain is provisioned at job start by `mise install`.

**Tech Stack:** Rust 2024 (stable 1.97.1, pinned), tokio, clap, serde/serde_json,
anyhow, tempfile, nix (signals), git/docker/kubectl via subprocess. Docker
buildx + GitHub Actions for the image. jj (colocated with git).

**Spec:** `docs/superpowers/specs/2026-09-11-software-factory-v2.md` — amended
by Task 1 with every decision below. Read the amended spec before Task 2.

## Decisions this plan implements

Agreed in the F2 design interview on 2026-09-21. Reopening one is a design
change, not a clarification.

| # | Decision |
|---|---|
| 1 | The whole job runs inside the boundary as hidden `assembly job-exec`. The host never touches a checkout. |
| 2 | Every runner clones from the remote. A job can only start from a ref the remote has. |
| 3 | Push or fail: a push that fails after retries fails the job. No remote is refused before launch. `pushed_to` stays `Option<String>`; `None` appears only in pre-F2 logs. |
| 4 | Stdout carries envelope frames `{seq, event}` / `{seq, output}`. `job-exec` wraps all agent and `verify` output, so an agent cannot forge an event. Non-frame lines go to the log. The collector dedupes by `seq` and records `JobFailed` itself when the runner dies without a verdict. |
| 5 | Attached only in F2. Reattach is F3; `seq` makes it additive. |
| 6 | The host resolves a SHA-pinned `JobPayload`, passed in the `ASSEMBLY_JOB` env var. `job-exec` never reads config. |
| 7 | One `Runner` trait with a per-implementor `RunningJob` — local, docker, k8s. |
| 8 | Docker and k8s drive the `docker`/`kubectl` CLIs, with a preflight per runner. Tested with shell-script fakes; no feature gate. |
| 9 | `--runner local\|docker\|k8s` (default local), `--image` (default: the published image at this binary's version), `--pass-env NAME`. k8s **requires** `--namespace`; `--context` is optional. The runner is not recorded in `meta.json`. |
| 10 | The host decides what crosses into a container: `ASSEMBLY_GIT_TOKEN` always (SSH remotes rewritten to HTTPS), anything else only via `--pass-env`. Docker `-e NAME`; k8s a per-job Secret owned by the Job. |
| 11 | Short-lived token minting moves to F4, as a stretch goal. |
| 12 | One published image: `assembly`, `git`, `mise`, and Claude Code, Codex and opencode. |
| 13 | The repository's toolchain comes from `mise install`, containers only (`provision_toolchain`), under its own cap. |
| 14 | No `setup` field. The system-package gap stays open until mise-nix, which is F8 — after F7, not part of F2. |
| 15 | `copy` is local-only; container runners refuse it in preflight. |
| 16 | Docker gets a named `mise` cache volume; k8s provisions cold in F2. |
| 17 | k8s scheduling deadline 10m; provisioning cap 15m; `activeDeadlineSeconds` = 2 × `max_duration` + 30m. |
| 18 | `gc`, worktrees and `ASSEMBLY_WORKTREE_ROOT` are deleted. |
| 19 | Dockerfile, `just image`, `just smoke-docker`, and a tag-triggered multi-arch GHCR publish. The rest of CI is out of scope. |

## Global Constraints

Copied from `CLAUDE.md` and the spec. Every task's requirements implicitly
include this section.

- **Edition 2024, pinned to stable 1.97.1.** Never downgrade the edition to
  work around a compile error — fix the code.
- **All file modifications go through the Edit / Write tools.** Never `sed -i`,
  `perl -pi`, heredocs, `cat >`, `tee`, or inline scripts that write files.
  Reading and searching via shell is fine. Deleting a whole file the task
  says to delete is `rm` — that is file management, not editing.
- **Every change in the jj stack must build, test, lint and format cleanly on
  its own.** `just check` (= `fmt-check` + `lint` + `test`) is the gate.
- **One concern per change**, each carrying the tests for what it adds or
  changes. A test file belongs to the change that introduces the module it
  imports.
- **Functional by default.** Iterators over `for` loops; `match` on the shape
  of data over if-else chains; build values rather than mutating them. Loops
  are correct for sequential I/O with early return — the collector's read
  loop and the push retry below are such loops, and each says so.
- **Naming.** Predicates read as claims. Filters name what they select. Error
  producers name the fault. Mutators name the transition including its scope.
  Fields carry their unit or role. Constructors say where the value came from.
- **No trait until a second implementor exists.** The `Runner` trait arrives
  in Task 7, with docker — not in Task 6, where local is alone.
- **`async fn` in a public trait trips `async_fn_in_trait`** under
  `-D warnings`. Write trait methods as `fn ... -> impl Future<Output = T> + Send`.
- **Validation returns a `Vec` of typed errors**, and every error's `Display`
  says what to do about it.
- **The event log is append-only.** `JobState::replay` and
  `JobReport::from_events` stay pure folds.
- **The target repository's working tree is never modified.** Fetching into
  its `.git` is allowed; writing files outside `.assembly/jobs/` is not.
- **No test may touch the network, a real Docker daemon, a real cluster, or
  require credentials.** Agents are shell-script fakes in `tests/fixtures/`;
  `docker`, `kubectl`, `mise` and `gh` are shell-script fakes written by the
  test that needs them.
- **Test counts climb monotonically**, except in Task 3 and Task 4, which
  delete the worktree and `gc` suites. Record the count in every commit body.

## Version control

jj, colocated with git. After each task:

```bash
jj describe -m "<conventional commit subject>

<body: what this changes, and the test count after>"
jj new
```

Never `jj edit` down the stack to verify. `just verify-stack` exports each
revision with `git archive` and builds it in a temp directory.

## The stack

| Task | Change | Behavior change |
|---|---|---|
| 1 | `docs(spec)`: amend the spec with the F2 decisions | — |
| 2 | `feat(frame)`: the wire envelope and the pure collection fold | none |
| 3 | `refactor(job)!`: a round runs in a scratch clone of the remote; push or fail | **yes** |
| 4 | `refactor!`: delete worktrees and `gc` | **yes** — `assembly gc` removed |
| 5 | `refactor(payload)`: the host resolves a serialisable `JobPayload` | none |
| 6 | `feat(job-exec)`: the job crosses a process boundary — `job-exec`, the local runner, the collector | none visible |
| 7 | `feat(runner)`: the `Runner` trait, the docker runner, `--runner/--image/--pass-env` | new runner |
| 8 | `feat(job-exec)`: `provision_toolchain` runs `mise install` under its own cap | containers only |
| 9 | `feat(runner)`: the k8s runner | new runner |
| 10 | `build(image)`: Dockerfile, `just image`, `just smoke-docker` | — |
| 11 | `ci`: tag-triggered multi-arch publish to GHCR | — |

Two deviations from the order agreed in the interview, both for the stack's
own rules: **clone before payload** (a SHA-pinned payload is meaningless
until jobs clone from the remote), and **`job-exec` + local runner as one
change** (introducing the trait with one implementor would violate
`CLAUDE.md`, so it waits for docker in Task 7).

## File Structure

What each file is responsible for once F2 is done.

| File | Responsibility after F2 |
|---|---|
| `src/cli.rs` | `run`, `revise`, `status`, `logs`, hidden `job-exec`; `RunnerArgs` |
| `src/config.rs` | `RepoConfig` — unchanged |
| `src/event.rs` | Event vocabulary; append-only log; `append_collected` |
| `src/frame.rs` | **new** — `Frame`, `FrameWriter`, `StreamPosition`, `verdict_missing_from` |
| `src/collect.rs` | **new** — the host-side collector: stream → `events.jsonl` + log |
| `src/payload.rs` | **new** — `JobPayload`, `ASSEMBLY_JOB`, `ASSEMBLY_GIT_TOKEN`, `https_equivalent` |
| `src/runner/mod.rs` | **new** — `Runner`, `RunningJob`, `Termination`, `JobSecrets`, `RunnerProblem` |
| `src/runner/child.rs` | **new** — `ChildLines`: a child's merged stdout+stderr as lines |
| `src/runner/local.rs` | **new** — `LocalRunner` |
| `src/runner/docker.rs` | **new** — `DockerRunner` |
| `src/runner/kubernetes.rs` | **new** — `KubernetesRunner`, manifests, `pod_progress` |
| `src/job.rs` | `run_round` — one round inside the boundary, writing frames |
| `src/exec.rs` | Running a command with a timeout, its output framed |
| `src/workspace.rs` | A scratch clone: create, commit, publish, discard |
| `src/git.rs` | Git subprocesses; clone/fetch added, worktree ops removed |
| `src/paths.rs` | Job directories and `meta.json` only |
| `src/delivery.rs` | Opening a pull request for an already-pushed branch |
| `src/main.rs` | CLI wiring, runner dispatch, printing |
| **deleted** | `src/gc.rs` |
| `Dockerfile`, `scripts/smoke-docker.sh`, `.github/workflows/publish-image.yml` | **new** |

---

### Task 1: Amend the spec

Records the interview's decisions where every later task can argue from
them. No code.

**Files:**
- Modify: `docs/superpowers/specs/2026-09-11-software-factory-v2.md`

**Interfaces:** none.

- [ ] **Step 1: Add an amendment note under the status line**

```markdown
**Amended:** 2026-09-21 — F2 design interview. Runner implementors, the wire
envelope, credentials, the job image and `copy` are settled below; see
`docs/superpowers/plans/2026-09-21-software-factory-f2.md` for the decision
table.
```

- [ ] **Step 2: Rewrite "The runner" section's table and trait paragraph**

Replace the three-row table and the sentence after it with:

```markdown
| Implementor | Isolation | Launched by | Event stream |
|---|---|---|---|
| Local process | None | spawning `assembly job-exec` | the child's stdout |
| `docker run` | Container | the `docker` CLI | the attached `docker run`'s stdout |
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

**What runs inside the boundary is the whole job.** A hidden
`assembly job-exec` clones the remote into scratch, runs the agent, commits,
runs `verify`, pushes, and reports. The host never touches a checkout, so
every runner — local included — clones, and a job can only start from a ref
the remote has.
```

- [ ] **Step 3: Rewrite "Reporting is NDJSON on stdout"**

Replace the paragraph beginning "The runner emits assembly-line's own event
schema" with:

```markdown
The runner emits assembly-line's own event schema — the schema in
`src/event.rs`, unchanged — inside an envelope, one frame per line:

    {"seq":1,"event":{"at":"…","t":"job_started","round":1}}
    {"seq":2,"output":"fake-agent: writing the file"}

`job-exec` pipes the agent's and `verify`'s output to itself and re-emits
each line as an `output` frame, so nothing an agent prints can arrive as an
`event`. Lines that are not frames — `job-exec`'s own stderr, merged in by
k8s — go to the log. `seq` numbers every frame, so a collector that resumes a
dropped stream drops what it already has. A stream that ends without a
verdict gets one from the collector: `JobFailed` naming why the runner
stopped.

The host resolves everything a job needs — config read from the base ref,
the provider command, `verify`, timeouts — into a payload pinned to a commit
SHA, and passes it in the `ASSEMBLY_JOB` environment variable. The job never
reads config, so nothing inside the boundary can influence its own plan.
```

- [ ] **Step 4: Rewrite "Credentials"**

Replace the section body with:

```markdown
The daemon is the only credential holder, and the holder decides what
leaves. A container job always receives `ASSEMBLY_GIT_TOKEN`, which
`job-exec` wires into a git credential helper for its clone and push, and
never exposes to the agent; SSH remotes are rewritten to HTTPS for it.
Anything else the agent needs — its API key — is named by the host
(`--pass-env` in F2, daemon config from F3). Docker receives values as
`-e NAME`, never on a command line; k8s as a per-job Secret owned by the Job
and deleted with it. The local runner inherits the host's environment, as it
has no isolation to preserve.

**"Per-job" in F2 means scoped to the job's lifetime, not short-lived.**
Minting short-lived git credentials needs a token issuer — realistically a
GitHub App — which is F4 machinery; it is an F4 stretch goal.

Jobs push their own branches, so a pushed branch stays durable even if the
daemon dies mid-job. **A job that cannot push fails:** its only local ref is
in a scratch clone that is deleted with it.
```

Keep the existing "The alternative the old spec named — clone read-only,
bundle back" paragraph and the "Assumption, stated" paragraph unchanged
after it.

- [ ] **Step 5: Add a "The job image" section after "Credentials"**

```markdown
### The job image

One published image, `ghcr.io/hmbill694/assembly-line:<version>`, carrying
`assembly`, `git`, `mise`, and the Claude Code, Codex and opencode CLIs. The
runner launches the image whose tag is its own version, so the collector and
`job-exec` never skew. Users never build an image.

A repository's toolchain — which `verify` and the agent both need — is
provisioned at job start by `mise install`, from files repositories already
carry: `mise.toml`, `.tool-versions`, `.nvmrc`, `.python-version`,
`rust-toolchain.toml`, `go.mod`. Provisioning runs in containers only; the
local runner uses the host's toolchain. System packages are out of reach
until the mise-nix backend lands in F8; there is
deliberately no `setup` field to fill that gap in the meantime.

`copy` is local-only in F2: its files come from a human's checkout, which a
container does not have. Container runners refuse a repository that declares
it. Where those files come from for a daemon is decided in F3.
```

- [ ] **Step 6: Amend Testing, What survives, Invariants, Milestones, Deferred**

In **Testing**, replace "The one exception to record" paragraph with:

```markdown
**No exception.** Every runner, k8s included, is tested against
shell-script fakes of the CLI it drives. Only a real-cluster smoke test
would need a cluster, and none is part of the suite.
```

In **What survives, promoted**, change the `src/workspace.rs` row to
"`copy` seeding into a scratch clone" and add a row to **What this deletes**:
"`src/gc.rs`, worktrees, `ASSEMBLY_WORKTREE_ROOT` | Jobs clone into scratch
that is deleted with them; nothing is left to collect".

In **Invariants**, change "Worktrees are scratch and always removed" to
"Checkouts are scratch and always removed".

In **Milestones**, replace the F2 row, and add a new row at the *end* of the
table, after F7:

```markdown
| **F2** | The runner seam. One trait; local, `docker run`, and k8s Job implementors driving their CLIs. The whole job inside the boundary as `job-exec`; clone, push or fail. NDJSON frames on stdout. Host-resolved payload. Host-chosen per-job credentials. One published image; `mise` provisioning. |
```

```markdown
| **F8** | mise-nix: Nix in the image; system packages declared in a repository's `mise.toml`. Closes the gap F2 leaves open, once the factory itself is finished. |
```

and append to the F4 row: "Stretch: short-lived, per-job git credentials
minted from a GitHub App."

In **Deferred, knowingly**, delete the "Container image strategy" and "How
`copy`-seeded files reach a remote runner" bullets, and add:

```markdown
- A cache for k8s `mise` provisioning (a PVC, node-local storage) — a
  cluster-operator choice for daemon config in F3. F2 provisions cold.
- Reattaching to a job after the collector restarts. `seq` makes it
  additive; the daemon (F3) is the first long-lived collector.
```

In **Accepted risks**, append:

```markdown
9. **System packages are unavailable to container jobs** until F8's
   mise-nix backend. A repository that needs one runs on the local runner.
10. **k8s jobs provision their toolchain cold**, costing minutes per job,
    until F3 configures a cache.
```

- [ ] **Step 7: Commit**

```bash
jj describe -m "docs(spec): F2 decisions — runners drive CLIs, frames, one image

Records the 2026-09-21 design interview: the whole job runs as job-exec,
every runner clones and pushes or fails, stdout carries seq-numbered frames,
the host resolves a pinned payload, credentials cross only when the host
names them, one image provisions toolchains with mise, copy is local-only,
gc is deleted. Minting moves to F4 as a stretch goal; mise-nix becomes F8,
after the factory is finished.

Tests: unchanged."
jj new
```

---

### Task 2: The wire envelope

Pure types and a pure fold. Nothing uses them yet; Task 6 does.

**Files:**
- Create: `src/frame.rs`
- Modify: `src/lib.rs` (add `pub mod frame;`)
- Test: `tests/frame.rs`

**Interfaces:**
- Consumes: `event::{Event, EventKind}`.
- Produces:
  - `pub struct Frame { pub seq: u64, pub body: FrameBody }` (serde, `body` flattened)
  - `pub enum FrameBody { Event(Event), Output(String) }`
  - `pub struct FrameWriter<W: Write>` — `Clone`; `new(W)`,
    `append_event(&self, EventKind) -> io::Result<Event>`,
    `append_output(&self, &str) -> io::Result<()>`,
    `copy_of_sink(&self) -> W where W: Clone`
  - `pub enum Routed { Event { seq: u64, event: Event }, Output(String), AlreadyCollected }`
  - `pub struct StreamPosition` — `Default`, `Copy`; `route(self, &str) -> (StreamPosition, Routed)`
  - `pub fn verdict_missing_from(collected: &[Event], ended_because: &str) -> Option<EventKind>`

- [ ] **Step 1: Write the failing tests**

`tests/frame.rs`:

```rust
use assembly_line::event::{Event, EventKind};
use assembly_line::frame::{FrameWriter, Routed, StreamPosition, verdict_missing_from};

fn lines_of(frames: &FrameWriter<Vec<u8>>) -> Vec<String> {
    String::from_utf8(frames.copy_of_sink())
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

/// Route every line through a fresh position, as a collector would.
fn routed(lines: &[String]) -> Vec<Routed> {
    lines
        .iter()
        .scan(StreamPosition::default(), |position, line| {
            let (next, routed) = position.route(line);
            *position = next;
            Some(routed)
        })
        .collect()
}

#[test]
fn an_event_survives_the_trip_through_a_frame() {
    let frames = FrameWriter::new(Vec::new());
    let written = frames
        .append_event(EventKind::JobStarted { round: 2 })
        .unwrap();

    match routed(&lines_of(&frames)).as_slice() {
        [Routed::Event { seq: 1, event }] => assert_eq!(event, &written),
        other => panic!("expected one event frame, got {other:?}"),
    }
}

#[test]
fn frames_are_numbered_from_one_in_the_order_written() {
    let frames = FrameWriter::new(Vec::new());
    frames.append_output("first").unwrap();
    frames.append_event(EventKind::JobStarted { round: 1 }).unwrap();
    frames.append_output("third").unwrap();

    let seqs: Vec<u64> = lines_of(&frames)
        .iter()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()["seq"].as_u64().unwrap())
        .collect();
    assert_eq!(seqs, [1, 2, 3]);
}

#[test]
fn each_frame_is_one_json_object_keyed_by_what_it_carries() {
    let frames = FrameWriter::new(Vec::new());
    frames.append_event(EventKind::JobStarted { round: 1 }).unwrap();
    frames.append_output("hello").unwrap();

    let lines = lines_of(&frames);
    let event: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    let output: serde_json::Value = serde_json::from_str(&lines[1]).unwrap();
    assert_eq!(event["event"]["t"], "job_started");
    assert_eq!(output["output"], "hello");
}

/// The property the envelope exists for: an agent printing an event verbatim
/// produces text in the log, never a verdict in the event stream.
#[test]
fn output_that_looks_exactly_like_an_event_is_still_output() {
    let forged = serde_json::to_string(&Event {
        at: chrono::Utc::now(),
        kind: EventKind::JobFinished { exit_code: 0 },
    })
    .unwrap();
    let frames = FrameWriter::new(Vec::new());
    frames.append_output(&forged).unwrap();

    assert_eq!(routed(&lines_of(&frames)), [Routed::Output(forged)]);
}

#[test]
fn a_line_that_is_not_a_frame_is_output() {
    let lines = vec!["thread 'main' panicked at src/main.rs:1".to_string()];
    assert_eq!(
        routed(&lines),
        [Routed::Output("thread 'main' panicked at src/main.rs:1".into())]
    );
}

/// A k8s log stream resumed with `--since-time` replays from an earlier
/// point; what the collector already has must not be appended twice.
#[test]
fn a_replayed_frame_is_recognised_as_already_collected() {
    let frames = FrameWriter::new(Vec::new());
    frames.append_event(EventKind::JobStarted { round: 1 }).unwrap();
    frames.append_output("working").unwrap();
    let lines = lines_of(&frames);
    let replayed: Vec<String> = lines.iter().chain(lines.iter()).cloned().collect();

    let routes = routed(&replayed);
    assert!(matches!(routes[0], Routed::Event { seq: 1, .. }));
    assert_eq!(routes[1], Routed::Output("working".into()));
    assert_eq!(routes[2], Routed::AlreadyCollected);
    assert_eq!(routes[3], Routed::AlreadyCollected);
}

fn event(kind: EventKind) -> Event {
    Event { at: chrono::Utc::now(), kind }
}

#[test]
fn a_round_that_reported_its_verdict_needs_nothing_added() {
    let passed = [
        event(EventKind::JobStarted { round: 1 }),
        event(EventKind::JobFinished { exit_code: 0 }),
    ];
    let failed = [
        event(EventKind::JobStarted { round: 1 }),
        event(EventKind::JobFailed { reason: "exit 3".into() }),
    ];
    assert_eq!(verdict_missing_from(&passed, "exit 0"), None);
    assert_eq!(verdict_missing_from(&failed, "exit 1"), None);
}

/// A pod OOM-killed mid-round never prints its verdict. The collector writes
/// one, naming why the runner stopped, so the job is not left `running`
/// forever.
#[test]
fn a_round_that_ended_without_a_verdict_is_failed_with_the_runners_reason() {
    let cut_short = [event(EventKind::JobStarted { round: 1 })];

    match verdict_missing_from(&cut_short, "OOMKilled") {
        Some(EventKind::JobFailed { reason }) => assert!(reason.contains("OOMKilled"), "{reason}"),
        other => panic!("expected a JobFailed, got {other:?}"),
    }
}

#[test]
fn a_runner_that_never_started_the_round_still_leaves_a_failure() {
    assert!(matches!(
        verdict_missing_from(&[], "the pod never started: ImagePullBackOff"),
        Some(EventKind::JobFailed { .. })
    ));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --test frame`
Expected: FAIL — `unresolved import assembly_line::frame`.

- [ ] **Step 3: Implement `src/frame.rs`**

```rust
//! The wire format between a job and whoever collects it.
//!
//! A job runs somewhere its collector cannot see — a child process, a
//! container, a pod on another machine — and the one thing all of those
//! share is stdout. Every line a job prints there is a [`Frame`]: one of its
//! [`Event`]s, or one line of what its commands printed.
//!
//! The job wraps its commands' output itself, so nothing an agent prints can
//! arrive as an `event` frame: an agent echoing `{"t":"job_finished"}` lands
//! in the log as text, not in the event stream as a verdict.

use crate::event::{Event, EventKind};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frame {
    /// Position in the job's stream, from 1. A collector that has to
    /// reconnect replays from an earlier point, and `seq` is what lets it
    /// drop what it already has.
    pub seq: u64,
    #[serde(flatten)]
    pub body: FrameBody,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameBody {
    Event(Event),
    /// One line a command printed, on stdout or stderr.
    Output(String),
}

#[derive(Debug)]
struct Numbered<W> {
    sink: W,
    last_seq: u64,
}

/// The job's side of the stream. Cloning shares it: the job appends events
/// while the readers forwarding its commands' stdout and stderr append
/// output, and all of them draw from one sequence.
#[derive(Debug)]
pub struct FrameWriter<W: Write> {
    shared: Arc<Mutex<Numbered<W>>>,
}

impl<W: Write> Clone for FrameWriter<W> {
    fn clone(&self) -> Self {
        FrameWriter {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<W: Write> FrameWriter<W> {
    pub fn new(sink: W) -> Self {
        FrameWriter {
            shared: Arc::new(Mutex::new(Numbered { sink, last_seq: 0 })),
        }
    }

    /// Append one event, stamped now, and flush it.
    ///
    /// # Errors
    ///
    /// Returns an error if the frame cannot be written or flushed. The caller
    /// should treat this as fatal: an event that never left the job is an
    /// event the collector can never record.
    pub fn append_event(&self, kind: EventKind) -> io::Result<Event> {
        let event = Event { at: Utc::now(), kind };
        self.append(FrameBody::Event(event.clone()))?;
        Ok(event)
    }

    /// Append one line a command printed.
    ///
    /// # Errors
    ///
    /// Returns an error if the frame cannot be written or flushed.
    pub fn append_output(&self, line: &str) -> io::Result<()> {
        self.append(FrameBody::Output(line.to_string()))
    }

    fn append(&self, body: FrameBody) -> io::Result<()> {
        let mut numbered = self
            .shared
            .lock()
            .map_err(|_| io::Error::other("a frame writer panicked mid-write"))?;
        let frame = Frame {
            seq: numbered.last_seq + 1,
            body,
        };
        let line = serde_json::to_string(&frame).map_err(io::Error::other)?;
        numbered.sink.write_all(line.as_bytes())?;
        numbered.sink.write_all(b"\n")?;
        numbered.sink.flush()?;
        numbered.last_seq = frame.seq;
        Ok(())
    }

    /// What has been written so far — for tests, which write into a `Vec`.
    ///
    /// # Panics
    ///
    /// Panics if a writer panicked while holding the lock.
    #[must_use]
    pub fn copy_of_sink(&self) -> W
    where
        W: Clone,
    {
        self.shared.lock().expect("frame writer lock").sink.clone()
    }
}

/// What a collector does with one line of a job's stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Routed {
    /// Append to `events.jsonl`, as the job recorded it.
    Event { seq: u64, event: Event },
    /// Append to the job's log.
    Output(String),
    /// A frame at or before one already routed — a resumed stream replaying.
    AlreadyCollected,
}

/// How far into a job's stream a collector has got.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StreamPosition {
    last_seq: u64,
}

impl StreamPosition {
    /// Where `line` goes, and where the stream stands after it.
    ///
    /// A line that is not a frame is output: `job-exec`'s own stderr, or a
    /// crash backtrace, merged into the stream by a runner that cannot keep
    /// the two apart. It does not move the position.
    #[must_use]
    pub fn route(self, line: &str) -> (StreamPosition, Routed) {
        match serde_json::from_str::<Frame>(line) {
            Err(_) => (self, Routed::Output(line.to_string())),
            Ok(frame) if frame.seq <= self.last_seq => (self, Routed::AlreadyCollected),
            Ok(Frame { seq, body }) => (
                StreamPosition { last_seq: seq },
                match body {
                    FrameBody::Event(event) => Routed::Event { seq, event },
                    FrameBody::Output(text) => Routed::Output(text),
                },
            ),
        }
    }
}

/// The failure a collector records itself when a round's stream ended
/// without the job saying how the round went — a pod OOM-killed, a runner
/// that never started it, a `job-exec` that panicked. `None` when the job
/// reported its own verdict.
///
/// `collected` is this round's events only: an earlier round's verdict says
/// nothing about this one.
#[must_use]
pub fn verdict_missing_from(collected: &[Event], ended_because: &str) -> Option<EventKind> {
    let reported = collected.iter().any(|e| {
        matches!(
            e.kind,
            EventKind::JobFinished { .. } | EventKind::JobFailed { .. }
        )
    });

    (!reported).then(|| EventKind::JobFailed {
        reason: format!("the job ended without reporting a verdict: {ended_because}"),
    })
}
```

Add `pub mod frame;` to `src/lib.rs` in alphabetical position.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test --test frame`
Expected: PASS (9 tests). Then `just check`.

- [ ] **Step 5: Commit**

```bash
jj describe -m "feat(frame): the envelope a job reports itself in

Every line a job prints on stdout is a seq-numbered frame carrying either an
event or one line of output. The job wraps command output itself, so an
agent cannot forge an event. StreamPosition routes lines and drops replayed
frames; verdict_missing_from names the failure a collector records when a
stream ends without one. Nothing uses these yet.

Tests: <count>."
jj new
```

---

### Task 3: A round runs in a scratch clone of the remote

The behavior change of F2, landed while everything else is still in-process.
A job fetches its start from the remote, clones into scratch, and pushes or
fails. Delivery stops pushing — the job already did.

**Files:**
- Modify: `Cargo.toml` (move `tempfile = "3"` from `[dev-dependencies]` to `[dependencies]`)
- Modify: `src/git.rs`, `src/workspace.rs`, `src/job.rs`, `src/event.rs`
  (doc comment only), `src/delivery.rs`, `src/main.rs`
- Test: `tests/support/mod.rs`, `tests/job.rs`, `tests/workspace.rs`,
  `tests/verify.rs`, `tests/delivery.rs`, `tests/cli.rs`, `tests/git.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces:
  - `git::PinnedRef { pub name: String, pub sha: String }` (derive `Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize`)
  - `git::remote_url(repo, remote: &str) -> anyhow::Result<Option<String>>`
  - `git::fetched_sha(repo, remote: &str, git_ref: &str) -> anyhow::Result<String>`
  - `git::pinned(repo, remote: &str, git_ref: &str) -> anyhow::Result<PinnedRef>`
  - `git::clone_into(url: &str, into: &Path) -> anyhow::Result<()>`
  - `git::check_out_new_branch(clone, branch: &str, at: &str) -> anyhow::Result<()>`
  - `workspace::JobWorkspace` (private `TempDir`; `path(&self) -> &Path`; pub `branch`, `seeded`)
  - `workspace::create(remote_url, start: &PinnedRef, branch, seed_from, copy_paths, scratch_root) -> anyhow::Result<JobWorkspace>`
  - `workspace::publish(ws: &JobWorkspace) -> anyhow::Result<()>`
  - `workspace::discard(ws: JobWorkspace) -> io::Result<()>`
  - `job::RunOpts { cancel, repo, seed_from, remote, scratch_root: PathBuf }`
  - `job::JobSpec { prompt, provider, start: &PinnedRef, round }` — `base_ref` is gone
  - `delivery::deliver(repo, delivery, job_branch, base) -> Delivered` — no remote, no push, infallible

- [ ] **Step 1: Write the failing git tests**

Append to `tests/git.rs`:

```rust
#[tokio::test]
async fn a_remotes_url_is_read_back_and_a_missing_remote_is_none() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    support::init_git_repo(&repo).await;
    assert_eq!(git::remote_url(&repo, "origin").await.unwrap(), None);

    let origin = tmp.path().join("origin.git");
    support::add_origin(&repo, &origin).await;
    assert_eq!(
        git::remote_url(&repo, "origin").await.unwrap().as_deref(),
        Some(origin.to_str().unwrap())
    );
}

/// A job starts from what the remote says a ref is, not what the local
/// repository says: unpushed work is not something a clone can see.
#[tokio::test]
async fn a_ref_is_pinned_to_the_commit_the_remote_has_for_it() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    support::init_git_repo(&repo).await;
    support::add_origin(&repo, &tmp.path().join("origin.git")).await;
    support::publish_main(&repo).await;
    let pushed = head_sha(&repo).await.unwrap();

    std::fs::write(repo.join("unpushed.txt"), "local only\n").unwrap();
    commit_all(&repo, "unpushed").await.unwrap().unwrap();

    let pinned = git::pinned(&repo, "origin", "main").await.unwrap();
    assert_eq!(pinned, git::PinnedRef { name: "main".into(), sha: pushed });
}

#[tokio::test]
async fn a_ref_the_remote_does_not_have_cannot_be_pinned() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    support::init_git_repo(&repo).await;
    support::add_origin(&repo, &tmp.path().join("origin.git")).await;
    support::publish_main(&repo).await;

    let err = git::pinned(&repo, "origin", "never-pushed").await.unwrap_err();
    assert!(err.to_string().contains("never-pushed"), "{err}");
}

#[tokio::test]
async fn a_clone_checks_out_a_new_branch_at_the_commit_it_is_given() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    support::init_git_repo(&repo).await;
    let origin = tmp.path().join("origin.git");
    support::add_origin(&repo, &origin).await;
    support::publish_main(&repo).await;
    let at = head_sha(&repo).await.unwrap();

    let clone = tmp.path().join("clone");
    std::fs::create_dir_all(&clone).unwrap();
    git::clone_into(origin.to_str().unwrap(), &clone).await.unwrap();
    git::check_out_new_branch(&clone, "al/job-1", &at).await.unwrap();

    assert_eq!(head_sha(&clone).await.unwrap(), at);
    assert_eq!(
        git::current_branch(&clone).await.unwrap().as_deref(),
        Some("al/job-1")
    );
    assert!(clone.join("README.md").is_file());
}
```

Add to `tests/support/mod.rs`:

```rust
/// Push `main` to `origin`, so the remote has something a job can start from.
pub async fn publish_main(repo: &Path) {
    git::push_branch(repo, "origin", "main").await.unwrap();
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --test git`
Expected: FAIL — `remote_url`, `pinned`, `PinnedRef`, `clone_into`, `check_out_new_branch` not found.

- [ ] **Step 3: Implement the git additions**

In `src/git.rs`, change the module doc's first rule to: "The user's working
tree is never touched — all writes happen in scratch clones assembly-line
creates elsewhere." Add `.kill_on_drop(true)` to the `Command` in
`run_allowing_failure`, so a git call abandoned by a timeout (Task 8) does
not outlive it. Then add:

```rust
/// A ref name and the commit it named when the job was planned.
///
/// The name is kept because a clone fetches by name; the sha is what the job
/// actually starts from, so config read at planning time and the tree the job
/// runs on are the same commit even if the ref moves in between.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PinnedRef {
    pub name: String,
    pub sha: String,
}

/// The URL `remote` points at, or `None` when the repository has no such
/// remote.
pub async fn remote_url(repo: impl AsRef<Path>, remote: &str) -> anyhow::Result<Option<String>> {
    let out = run_allowing_failure(repo, &["remote", "get-url", remote]).await?;
    Ok(out.succeeded().then(|| out.stdout.trim().to_string()))
}

/// Fetch `git_ref` from `remote` and return the commit it names *there*.
///
/// Writes the fetched objects and `FETCH_HEAD` into the repository's `.git`,
/// never its working tree.
pub async fn fetched_sha(
    repo: impl AsRef<Path>,
    remote: &str,
    git_ref: &str,
) -> anyhow::Result<String> {
    let repo = repo.as_ref();
    run_expecting_success(
        repo,
        &["fetch", "--quiet", remote, git_ref],
        &format!("fetch {remote} {git_ref}"),
    )
    .await?;
    run_expecting_success(repo, &["rev-parse", "FETCH_HEAD^{commit}"], "rev-parse FETCH_HEAD")
        .await
}

/// `git_ref` as `remote` has it, pinned to one commit.
///
/// # Errors
///
/// Beyond the usual, an error naming the ref when the remote does not carry
/// it — a job can only start from what a clone of the remote can see.
pub async fn pinned(
    repo: impl AsRef<Path>,
    remote: &str,
    git_ref: &str,
) -> anyhow::Result<PinnedRef> {
    fetched_sha(repo, remote, git_ref)
        .await
        .map(|sha| PinnedRef {
            name: git_ref.to_string(),
            sha,
        })
        .map_err(|e| {
            anyhow::anyhow!(
                "'{git_ref}' is not on '{remote}' — a job starts from a clone of the remote, \
                 so push it first: {e}"
            )
        })
}

/// Clone `url` into the existing, empty directory `into`, without checking
/// anything out — [`check_out_new_branch`] decides what the tree holds.
pub async fn clone_into(url: &str, into: impl AsRef<Path>) -> anyhow::Result<()> {
    run_expecting_success(into, &["clone", "--quiet", "--no-checkout", url, "."], "clone")
        .await
        .map(|_| ())
}

/// Create `branch` at `at` and check it out.
pub async fn check_out_new_branch(
    clone: impl AsRef<Path>,
    branch: &str,
    at: &str,
) -> anyhow::Result<()> {
    run_expecting_success(
        clone,
        &["checkout", "--quiet", "-b", branch, at],
        &format!("checkout -b {branch}"),
    )
    .await
    .map(|_| ())
}

/// Give a clone an author identity when the environment has none.
///
/// A container has no global git config, and a clone does not inherit the
/// source repository's local one; a commit without an identity is refused.
/// An identity the environment already provides is left alone, so local jobs
/// keep committing as the user.
pub async fn ensure_commit_identity(clone: impl AsRef<Path>) -> anyhow::Result<()> {
    let clone = clone.as_ref();
    let configured = run_allowing_failure(clone, &["config", "user.email"])
        .await?
        .succeeded();
    match configured {
        true => Ok(()),
        false => {
            run_expecting_success(clone, &["config", "user.name", "assembly-line"], "config")
                .await?;
            run_expecting_success(
                clone,
                &["config", "user.email", "assembly-line@localhost"],
                "config",
            )
            .await
            .map(|_| ())
        }
    }
}
```

`fetched_sha` fetches the ref *name*, so a raw SHA only works where the
server allows fetching one. Names — branches and tags — are what `--ref`
takes in practice; do not add a SHA fallback.

- [ ] **Step 4: Run the git tests**

Run: `cargo test --test git`
Expected: PASS.

- [ ] **Step 5: Rewrite `src/workspace.rs` around a scratch clone**

Replace the file with:

```rust
//! One job's sandbox: a scratch clone of the remote, optionally seeded with
//! files the repository does not carry.

use crate::git::{self, PinnedRef};
use std::path::Path;
use std::time::Duration;

#[derive(Debug)]
pub struct JobWorkspace {
    /// Deleted when the workspace is dropped, which is what makes the
    /// checkout scratch whatever becomes of the round.
    dir: tempfile::TempDir,
    pub branch: String,
    /// Relative paths copied in, which must never reach a commit.
    pub seeded: Vec<String>,
}

impl JobWorkspace {
    #[must_use]
    pub fn path(&self) -> &Path {
        self.dir.path()
    }
}

/// The remote a job clones from and publishes its branch to unless
/// configured otherwise.
pub const DEFAULT_REMOTE: &str = "origin";

/// A job's branch name. Git refs are paths, so this must never nest under
/// another ref assembly-line creates.
#[must_use]
pub fn job_branch_name(job_id: u64) -> String {
    format!("al/job-{job_id}")
}

/// Clone `remote_url` into a fresh directory under `scratch_root`, with
/// `branch` checked out at `start`, and seed it.
///
/// # Errors
///
/// Seed paths are checked *before* anything is cloned, so a typo leaves
/// nothing behind. A clone that fails midway leaves nothing either: the
/// directory is removed as the error propagates.
pub async fn create(
    remote_url: &str,
    start: &PinnedRef,
    branch: &str,
    seed_from: impl AsRef<Path>,
    copy_paths: &[String],
    scratch_root: impl AsRef<Path>,
) -> anyhow::Result<JobWorkspace> {
    let seed_from = seed_from.as_ref();

    if let Some(missing) = missing_seed_path(seed_from, copy_paths) {
        anyhow::bail!(
            "copy path '{missing}' does not exist under {}",
            seed_from.display()
        );
    }

    std::fs::create_dir_all(scratch_root.as_ref())?;
    let dir = tempfile::Builder::new()
        .prefix("assembly-job-")
        .tempdir_in(scratch_root)?;

    git::clone_into(remote_url, dir.path()).await?;
    // A tag, or a commit reachable only from the ref the job names, is not
    // guaranteed by a plain clone.
    git::run_allowing_failure(dir.path(), &["fetch", "--quiet", "origin", &start.name]).await?;
    git::check_out_new_branch(dir.path(), branch, &start.sha).await?;
    git::ensure_commit_identity(dir.path()).await?;
    seed_files(dir.path(), seed_from, copy_paths)?;

    Ok(JobWorkspace {
        dir,
        branch: branch.to_string(),
        seeded: copy_paths.to_vec(),
    })
}

/// The first `copy` path the repository declares that is not actually there.
fn missing_seed_path<'a>(seed_from: &Path, copy_paths: &'a [String]) -> Option<&'a String> {
    copy_paths.iter().find(|rel| !seed_from.join(rel).exists())
}

/// Copy the repository's `copy` paths into the checkout, creating whatever
/// directories they nest in.
fn seed_files(into: &Path, seed_from: &Path, copy_paths: &[String]) -> anyhow::Result<()> {
    copy_paths.iter().try_for_each(|rel| -> anyhow::Result<()> {
        let destination = into.join(rel);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(seed_from.join(rel), destination)
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!("copying '{rel}' into the workspace: {e}"))
    })
}

/// `None` means the agent changed nothing.
///
/// # Errors
///
/// See [`git::commit_all_except`].
pub async fn commit(ws: &JobWorkspace, message: &str) -> anyhow::Result<Option<String>> {
    git::commit_all_except(ws.path(), message, &ws.seeded).await
}

/// Waits before each push attempt. A transient network failure is worth
/// riding out; a refusal is not, but three quick attempts cost little.
const PUSH_BACKOFF: [Duration; 3] = [
    Duration::ZERO,
    Duration::from_millis(500),
    Duration::from_secs(2),
];

/// Push the job's branch to the clone's `origin`.
///
/// A loop rather than a combinator: each attempt is sequential I/O, and the
/// first success ends it.
///
/// # Errors
///
/// When every attempt fails, an error carrying git's last complaint and
/// saying what that costs: the branch exists only in this scratch clone, so
/// the work goes with it.
pub async fn publish(ws: &JobWorkspace) -> anyhow::Result<()> {
    let mut last_failure = None;
    for wait in PUSH_BACKOFF {
        tokio::time::sleep(wait).await;
        match git::push_branch(ws.path(), DEFAULT_REMOTE, &ws.branch).await {
            Ok(()) => return Ok(()),
            Err(e) => last_failure = Some(e),
        }
    }

    Err(anyhow::anyhow!(
        "could not publish {} — the work is lost with the scratch checkout: {}",
        ws.branch,
        last_failure.map(|e| e.to_string()).unwrap_or_default()
    ))
}

/// Remove the checkout. The branch lives on the remote.
///
/// # Errors
///
/// Returns an error if the directory cannot be removed.
pub fn discard(ws: JobWorkspace) -> std::io::Result<()> {
    ws.dir.close()
}
```

- [ ] **Step 6: Rewrite `src/job.rs`'s plumbing for clones**

Changes, in order:

1. `RunOpts` gains `pub scratch_root: PathBuf` with the doc comment
   "Where the scratch clone is made. The system temp directory in real runs;
   a directory the test owns in tests."

2. `JobSpec` replaces `pub base_ref: &'a str` with:

```rust
    /// Where this round starts: the base for round 1, the job's own branch
    /// tip for a revise round. Pinned by the caller, which has already read
    /// config from the same commit.
    pub start: &'a git::PinnedRef,
```

3. `AgentWork.pushed_to` becomes `String` (the doc comment: "The remote the
   branch reached. A round that could not push has no `AgentWork` at all.").

4. `JobPlan`: delete `repo`, `start: git::WorktreeStart` and
   `workspace_path`; add `remote_url: String`, `start: git::PinnedRef`,
   `scratch_root: PathBuf`. Keep `remote` (the host's name for it, which is
   what `JobBranchPublished` records).

5. Delete `ready_repository_for_worktrees` and `round_start`. Add:

```rust
/// Where the round clones from. A repository without the remote cannot run a
/// job at all: the job clones from it and pushes its branch back to it.
async fn remote_to_clone(repo: &Path, remote: &str) -> anyhow::Result<String> {
    git::remote_url(repo, remote).await?.ok_or_else(|| {
        anyhow::anyhow!(
            "the repository has no '{remote}' remote — a job clones from it and pushes its \
             branch back to it, so add one"
        )
    })
}
```

6. `job_plan` becomes `async` only if needed — it is not; pass the URL in:

```rust
fn job_plan(
    config: &RepoConfig,
    spec: &JobSpec<'_>,
    paths: &JobPaths,
    opts: &RunOpts,
    remote_url: String,
) -> anyhow::Result<JobPlan> {
    let provider = config
        .providers
        .get(spec.provider)
        .ok_or_else(|| ConfigError::UnknownProvider(spec.provider.to_string()))?;

    Ok(JobPlan {
        remote_url,
        start: spec.start.clone(),
        scratch_root: opts.scratch_root.clone(),
        branch: job_branch_name(paths.id),
        seed_from: opts.seed_from.clone(),
        copy_paths: config.copy.clone(),
        command: render_command(provider, spec.prompt),
        commit_message: commit_message(paths.id, spec.prompt),
        remote: opts.remote.clone(),
        verify: config.verify.clone(),
    })
}
```

7. `run_job`:

```rust
pub async fn run_job(
    config: &RepoConfig,
    spec: &JobSpec<'_>,
    paths: &JobPaths,
    log: &mut EventLog,
    opts: &RunOpts,
) -> anyhow::Result<JobOutcome> {
    let timeout = wall_clock_limit(config)?;
    let remote_url = remote_to_clone(&opts.repo, &opts.remote).await?;
    let plan = job_plan(config, spec, paths, opts, remote_url)?;

    log.append(EventKind::JobStarted { round: spec.round })?;

    let result = round_result(&plan, &paths.log(), timeout, opts.cancel.clone())
        .await
        // The round could not be administered at all — the clone failed, or
        // the push did and took the work with it. Either way there is no
        // branch to name.
        .unwrap_or_else(|e| RoundResult::Failed {
            reason: e.to_string(),
            work: None,
        });

    record_completion(log, result)
}
```

8. `revise_job` loses nothing but its `JobSpec` construction: it takes the
   pinned tip from its caller. Change its signature to add
   `start: &git::PinnedRef` after `revision`, and build
   `JobSpec { prompt: &prompt, provider: &meta.provider, start, round: revision.round }`.

9. `round_result`: create the workspace with

```rust
    let ws = workspace::create(
        &plan.remote_url,
        &plan.start,
        &plan.branch,
        &plan.seed_from,
        &plan.copy_paths,
        &plan.scratch_root,
    )
    .await?;
```

   run the agent and `verify` in `ws.path()`, and settle with
   `discarded: workspace::discard(ws).map_err(anyhow::Error::from)` — note
   `discard` now takes the workspace by value, so build `preserved` and
   `verdict` first (they borrow `ws`) and `discarded` last, as the struct
   literal already orders them. Bind `preserved` and `verdict` to locals
   before the literal so the borrow ends before the move.

10. `agent_work_on_branch`:

```rust
async fn agent_work_on_branch(
    plan: &JobPlan,
    ws: &JobWorkspace,
) -> anyhow::Result<Option<AgentWork>> {
    let Some(sha) = workspace::commit(ws, &plan.commit_message).await? else {
        return Ok(None);
    };
    let stat = git::diff_stat_against(ws.path(), &plan.start.sha).await?;
    workspace::publish(ws).await?;

    Ok(Some(AgentWork {
        branch: ws.branch.clone(),
        sha,
        stat,
        pushed_to: plan.remote.clone(),
    }))
}
```

11. Delete `remote_the_branch_reached`. In `work_recorded`, write
    `pushed_to: Some(w.pushed_to.clone())`.

12. Update the module doc: "Its checkout is a scratch clone and is discarded
    whatever happened, including on failure; its branch, pushed to the
    remote, is the whole durable output — which is why work is committed and
    pushed *before* success is decided, and why a round that cannot push
    fails."

- [ ] **Step 7: Reword `JobBranchPublished`'s doc comment**

In `src/event.rs`:

```rust
    /// The branch reached the remote. `pushed_to` names it.
    ///
    /// `pushed_to` is an `Option` only so logs written before F2 still
    /// parse: those recorded `None` for a branch that stayed a local ref.
    /// Since F2 a job's only local ref is in a scratch clone deleted with
    /// it, so a branch that cannot be pushed fails the round instead, and
    /// every new log carries `Some`.
    ///
    /// Emitted for failed jobs too. A job leaves nothing but its branch, so
    /// this is what makes the work findable at all.
```

- [ ] **Step 8: Delivery stops pushing**

In `src/delivery.rs`: remove the `remote` parameter, the `git` import, and
the push. The job pushed its own branch; delivery only asks for a pull
request.

```rust
/// Ask for a pull request from `job_branch` into `base`. The job already
/// pushed the branch, so nothing here can lose work.
pub async fn deliver(
    repo: impl AsRef<Path>,
    delivery: &Delivery,
    job_branch: &str,
    base: &str,
) -> Delivered {
    match delivery.mode {
        DeliveryMode::None => Delivered::Skipped("delivery mode is \"none\"".into()),
        DeliveryMode::Pr => open_pull_request(repo.as_ref(), job_branch, base).await,
    }
}
```

Update `Delivered`'s docs: `Skipped` — "Nothing was attempted, and this is
why: delivery is turned off."; `Pushed` — "The job pushed the branch, but no
pull request was opened — `gh` is not installed, or it refused."

- [ ] **Step 9: Pin in `main.rs`**

1. `default_base_ref`: a detached HEAD is now an error, because fetching
   `HEAD` from the remote would silently mean the remote's default branch:

```rust
/// What a job is cut from when the command line does not say: the branch the
/// repository has checked out, as the remote has it.
async fn default_base_ref(repo: &Path) -> Result<String, String> {
    match assembly_line::git::current_branch(repo).await {
        Ok(Some(branch)) => Ok(branch),
        Ok(None) => Err("HEAD is detached — name the ref to start from with --ref".into()),
        Err(e) => Err(e.to_string()),
    }
}
```

2. `PreparedJob` gains `start: git::PinnedRef`. In `prepare_job`, after
   `base_ref` is known:

```rust
    let start = assembly_line::git::pinned(&repo, DEFAULT_REMOTE, &base_ref)
        .await
        .map_err(|e| e.to_string())?;
    note_if_local_ref_differs(&repo, &base_ref, &start).await;

    let declared = RepoConfig::from_ref(&repo, &start.sha)
        .await
        .map_err(|e| e.to_string())?;
```

   with

```rust
/// A job starts from the remote's copy of a ref. When the user's own copy
/// differs — usually unpushed commits — say so, rather than let them wonder
/// where their work went.
async fn note_if_local_ref_differs(repo: &Path, base_ref: &str, start: &git::PinnedRef) {
    if let Ok(local) = assembly_line::git::sha_at_ref(repo, base_ref).await
        && local != start.sha
    {
        println!(
            "note: your '{base_ref}' is not what '{DEFAULT_REMOTE}' has — the job starts \
             from {DEFAULT_REMOTE}'s ({}); push first if you meant yours",
            &start.sha[..12.min(start.sha.len())]
        );
    }
}
```

3. `start_new_job` builds `JobSpec { prompt: &prompt, provider: &provider, start: &start, round: 1 }`.

4. `revise_existing_job` pins both refs before running:

```rust
    let base = git::pinned(&meta.repo, DEFAULT_REMOTE, &meta.base_ref)
        .await
        .map_err(|e| e.to_string())?;
    let tip = git::pinned(&meta.repo, DEFAULT_REMOTE, &job_branch_name(job_id))
        .await
        .map_err(|e| e.to_string())?;
    // `base`, not the job's own branch: the previous round is not allowed to
    // have changed the settings that govern this one.
    let declared = RepoConfig::from_ref(&meta.repo, &base.sha)
        .await
        .map_err(|e| e.to_string())?;
```

   and passes `&tip` to `revise_job`.

5. `machine_opts` sets `scratch_root: std::env::temp_dir()`.

6. `deliver_if_verified` calls `delivery::deliver(repo, &config.delivery, branch, base).await`
   and prints the result — no `match` on `Result` any more.

- [ ] **Step 10: Update the test harness**

In `tests/support/mod.rs`:

- `Harness` gains `pub origin: PathBuf` and a scratch dir. `with_config`
  always creates the origin and publishes `main` after the opt-in commit:

```rust
        let origin = tmp.path().join("origin.git");
        add_origin(&repo, &origin).await;
        publish_main(&repo).await;

        Harness { tmp, repo, origin }
```

- Delete `with_origin` (every harness has one) and the `Drop` impl's body
  still removes `paths::repo_worktrees_root` — leave `Drop` and
  `worktree_root` in place until Task 4 deletes them.
- Add:

```rust
    /// Where this harness's jobs make their scratch clones.
    pub fn scratch_root(&self) -> PathBuf {
        self.tmp.path().join("scratch")
    }

    /// Whether every scratch clone a job made is gone again.
    pub fn scratch_is_empty(&self) -> bool {
        std::fs::read_dir(self.scratch_root())
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(true)
    }

    /// What the remote's copy of `branch` carries at `path`, or `None`.
    pub async fn file_on_remote_branch(&self, branch: &str, path: &str) -> Option<String> {
        git::file_at_ref(&self.origin, branch, path).await.unwrap()
    }

    /// The files the remote's copy of `branch` carries.
    pub async fn files_on_remote_branch(&self, branch: &str) -> String {
        git::run_allowing_failure(&self.origin, &["ls-tree", "--name-only", "-r", branch])
            .await
            .unwrap()
            .stdout
    }
```

- `attempt_round` pins the start the same way `main.rs` does, reads config
  from the pinned sha, and sets `scratch_root`:

```rust
        let start = match round {
            1 => git::pinned(&self.repo, "origin", base_ref.unwrap_or("main")).await?,
            _ => git::pinned(&self.repo, "origin", &workspace::job_branch_name(THE_JOB)).await?,
        };
        let config = RepoConfig::from_ref(&self.repo, &start.sha).await?;
```

  (`repo_config()` stays for tests that only need the parsed config.) Build
  `JobSpec { prompt, provider: &provider, start: &start, round }` and
  `RunOpts { …, scratch_root: self.scratch_root() }`.

- [ ] **Step 11: Retarget the job, workspace, verify, delivery and CLI tests**

`tests/job.rs`:

- Every assertion that inspects the job's branch in `h.repo` now inspects
  the remote: `git::branch_exists(&h.repo, …)` becomes
  `!h.files_on_remote_branch(&branch).await.is_empty()`; `ls-tree`/`show`
  against `&h.repo` for the job branch become `h.files_on_remote_branch` /
  `h.file_on_remote_branch`.
- Every `!h.worktree_root().join("checkout").exists()` becomes
  `h.scratch_is_empty()`.
- `a_job_is_cut_from_the_ref_it_names_not_from_head`: push the tag and the
  later commit — `git push origin start-here` and `support::publish_main`
  after the later commit — before running `run_job_from("go", "start-here")`.
  The parent check reads `rev-parse {branch}^` in `h.origin`.
- `a_failed_jobs_branch_reaches_the_remote`: drop `h.with_origin()`; use
  `h.origin`.
- Replace `a_refused_push_still_records_the_branch_and_still_discards_the_checkout`
  with (keep its non-fast-forward setup verbatim, pushing to `h.origin`):

```rust
/// A job whose branch cannot leave the scratch clone has lost its work, and
/// must say so rather than record a branch that exists nowhere.
#[tokio::test]
async fn a_refused_push_fails_the_job_and_says_the_work_is_lost() {
    let h = Harness::new().await;
    // … the existing `unrelated` commit pushed to refs/heads/al/job-1 …

    let outcome = h.run_job("write a file").await;

    assert!(!outcome.succeeded);
    assert!(
        !outcome.has(|k| matches!(k, EventKind::JobBranchPublished { .. })),
        "a branch that never left the clone was recorded as published: {:?}",
        outcome.events
    );
    assert!(outcome.has(
        |k| matches!(k, EventKind::JobFailed { reason } if reason.contains("work is lost"))
    ));
    assert!(h.scratch_is_empty(), "the checkout leaked when the push failed");
    // … the existing check that origin's al/job-1 is still `unrelated` …
}
```

- Replace `publishing_without_a_remote_keeps_the_branch_local` with:

```rust
#[tokio::test]
async fn a_repository_with_no_remote_cannot_run_a_job() {
    let h = Harness::new().await;
    // Pin while the remote exists: pinning needs it too, and this test is
    // about the round's own check.
    let start = git::pinned(&h.repo, "origin", "main").await.unwrap();
    git::run_allowing_failure(&h.repo, &["remote", "remove", "origin"])
        .await
        .unwrap();

    let err = h.run_from(&start, "x").await.unwrap_err().to_string();
    assert!(err.contains("no 'origin' remote"), "{err}");
}
```

  `Harness::run_from(&start, prompt) -> anyhow::Result<Outcome>` does
  everything `attempt_round` does after pinning; refactor `attempt_round` to
  pin and then call `run_from`, so the two cannot drift.

- Delete `a_repository_with_no_commits_has_nothing_to_branch_from`: an empty
  remote cannot be pinned, so `git::pinned` already covers it
  (`a_ref_the_remote_does_not_have_cannot_be_pinned`).
- `a_revise_round_continues_the_branch_instead_of_starting_over`: read
  `rounds.txt` with `h.file_on_remote_branch(&branch, "rounds.txt")`.

`tests/workspace.rs`: rewrite around clones. Delete `fresh_branch_at` and the
two supersede tests (`a_second_attempt_supersedes…`,
`a_branch_left_without_its_worktree…`) — a scratch clone has no previous
attempt to clear. Keep the others, adapted to this fixture:

```rust
use assembly_line::git::{self, PinnedRef, commit_all, head_sha};
use assembly_line::workspace::{self, job_branch_name};
use std::path::PathBuf;

mod support;

/// A repository published to a bare origin, and a scratch root the test owns.
struct Fixture {
    tmp: tempfile::TempDir,
    repo: PathBuf,
    origin: PathBuf,
}

impl Fixture {
    async fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        support::init_git_repo(&repo).await;
        let origin = tmp.path().join("origin.git");
        support::add_origin(&repo, &origin).await;
        support::publish_main(&repo).await;
        Fixture { tmp, repo, origin }
    }

    fn url(&self) -> &str {
        self.origin.to_str().unwrap()
    }

    fn scratch(&self) -> PathBuf {
        self.tmp.path().join("scratch")
    }

    async fn main(&self) -> PinnedRef {
        git::pinned(&self.repo, "origin", "main").await.unwrap()
    }

    async fn workspace(&self, copy: &[String]) -> anyhow::Result<workspace::JobWorkspace> {
        workspace::create(self.url(), &self.main().await, "al/job-1", &self.repo, copy, self.scratch())
            .await
    }
}

#[tokio::test]
async fn creating_a_workspace_checks_out_the_pinned_commit_on_the_jobs_branch() {
    let fx = Fixture::new().await;
    let ws = fx.workspace(&[]).await.unwrap();

    assert_eq!(head_sha(ws.path()).await.unwrap(), fx.main().await.sha);
    assert_eq!(git::current_branch(ws.path()).await.unwrap().as_deref(), Some("al/job-1"));
    assert!(ws.path().starts_with(fx.scratch()));
}

#[tokio::test]
async fn discarding_a_workspace_removes_it_and_the_published_branch_survives() {
    let fx = Fixture::new().await;
    let ws = fx.workspace(&[]).await.unwrap();
    std::fs::write(ws.path().join("work.txt"), "done\n").unwrap();
    let sha = workspace::commit(&ws, "work").await.unwrap().unwrap();
    workspace::publish(&ws).await.unwrap();
    let path = ws.path().to_path_buf();

    workspace::discard(ws).unwrap();

    assert!(!path.exists());
    let on_remote = git::run_allowing_failure(&fx.origin, &["rev-parse", "al/job-1"]).await.unwrap();
    assert_eq!(on_remote.stdout.trim(), sha);
}

#[tokio::test]
async fn continuing_a_branch_restores_the_previous_rounds_work() {
    let fx = Fixture::new().await;
    let first = fx.workspace(&[]).await.unwrap();
    std::fs::write(first.path().join("rounds.txt"), "one\n").unwrap();
    workspace::commit(&first, "round 1").await.unwrap().unwrap();
    workspace::publish(&first).await.unwrap();
    workspace::discard(first).unwrap();

    let tip = git::pinned(&fx.repo, "origin", "al/job-1").await.unwrap();
    let second = workspace::create(fx.url(), &tip, "al/job-1", &fx.repo, &[], fx.scratch())
        .await
        .unwrap();

    assert_eq!(std::fs::read_to_string(second.path().join("rounds.txt")).unwrap(), "one\n");
}

#[tokio::test]
async fn a_refused_publish_names_the_branch_and_what_was_lost() {
    let fx = Fixture::new().await;
    let ws = fx.workspace(&[]).await.unwrap();
    std::fs::write(ws.path().join("work.txt"), "done\n").unwrap();
    workspace::commit(&ws, "work").await.unwrap().unwrap();
    // A remote that no longer exists refuses every attempt.
    std::fs::remove_dir_all(&fx.origin).unwrap();

    let err = workspace::publish(&ws).await.unwrap_err().to_string();
    assert!(err.contains("al/job-1"), "{err}");
    assert!(err.contains("work is lost"), "{err}");
}
```

Adapt the kept seeding and commit tests (`seeded_files_are_copied_in_and_kept_out_of_the_commit`,
`seeding_preserves_nested_paths`, `a_missing_seed_path_names_the_file_and_leaves_no_worktree`
→ rename `…_and_leaves_no_checkout` and assert `fx.scratch()` is empty or
absent, `committing_an_untouched_workspace_produces_nothing`) by replacing
their setup with `Fixture::new()` / `fx.workspace(&copy)` and `ws.path` with
`ws.path()`. Delete `discarding_a_workspace_removes_it_but_keeps_the_branch`
(superseded above).

`tests/verify.rs`: replace any `h.with_origin()` call (the harness always
has one) and any branch inspection against `h.repo` with the harness's
remote helpers.

`tests/delivery.rs`:

- `Fixture::with_origin` and `Fixture::job_branch` go; the library tests
  that needed them go too: `delivery_without_a_remote_is_skipped_rather_than_failing`,
  `delivery_turned_off_leaves_the_remote_alone`, and
  `pr_mode_pushes_the_branch_whether_or_not_gh_opens_a_pull_request` all test
  a push delivery no longer makes. `delivery_is_skipped_when_it_is_turned_off`
  becomes:

```rust
#[tokio::test]
async fn delivery_is_skipped_when_it_is_turned_off() {
    let fx = Fixture::new().await;
    let outcome = deliver(&fx.repo, &mode(DeliveryMode::None), "al/job-1", "main").await;
    assert!(matches!(outcome, Delivered::Skipped(_)), "{outcome:?}");
}
```

- The CLI helpers `repo_running` / `repo_running_with_base` add an origin
  **outside** the repository (so `git status` stays clean) and publish
  `main`. Replace `add_origin(&tmp)` with a helper every CLI test file uses:

```rust
/// Where this test's bare remote lives: outside the repository, so the
/// repository's own `git status` stays clean.
fn origin_for(tmp: &tempfile::TempDir) -> PathBuf {
    std::env::temp_dir().join(format!(
        "assembly-test-origin-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    ))
}
```

  called from `repo_running` right after the opt-in commit
  (`support::add_origin(tmp.path(), &origin_for(&tmp)).await; support::publish_main(tmp.path()).await;`),
  with `discard_worktrees` also removing `origin_for(tmp)`.
- `a_passing_revise_round_is_delivered`: there is no local `al/job-1` to
  compare with any more. Assert instead that stdout contains
  `pushed`-or-`opened` and not `not delivered` (as now), and that
  `origin_for(&tmp)`'s `al/job-1:rounds.txt` has two lines.

`tests/cli.rs`: same `origin_for` helper in `repo_running`, and the same
cleanup. `a_job_branches_from_the_ref_it_is_given` pushes its ref to the
origin before running and inspects the branch there. Delete
`gc_collects_a_jobs_worktrees_only_once_its_job_state_is_gone` and
`gc_older_than_collects_a_live_jobs_worktrees_but_only_when_asked`: jobs no
longer make worktrees, so their premise is false. `gc` itself goes in
Task 4. Add:

```rust
#[tokio::test]
async fn a_detached_head_is_asked_to_name_its_ref() {
    let tmp = repo_running("fake-agent.sh").await;
    git(&tmp, &["checkout", "--detach"]);

    assembly(&tmp)
        .args(["run", "--prompt", "x"])
        .assert()
        .code(2)
        .stderr(contains("--ref"));

    discard_worktrees(&tmp);
}

#[tokio::test]
async fn unpushed_local_work_is_pointed_out_and_the_remotes_ref_is_used() {
    let tmp = repo_running("fake-agent.sh").await;
    std::fs::write(tmp.path().join("unpushed.txt"), "mine\n").unwrap();
    commit_all(tmp.path(), "unpushed").await.unwrap().unwrap();

    assembly(&tmp)
        .args(["run", "--prompt", "x"])
        .assert()
        .success()
        .stdout(contains("push first"));

    discard_worktrees(&tmp);
}
```

- [ ] **Step 12: Run everything**

Run: `just check`
Expected: all green. The worktree tests in `tests/git.rs`, `tests/gc.rs` and
`tests/paths.rs` still pass — that code is unused by jobs now but still
exists until Task 4.

- [ ] **Step 13: Commit**

```bash
jj describe -m "refactor(job)!: a round runs in a scratch clone of the remote

A job pins its start to what the remote has for the ref, reads config from
that same commit, clones the remote into scratch, and pushes or fails: its
only local ref is in a clone deleted with it, so a branch that cannot leave
is lost work, and the round says so. Delivery stops pushing — the job
already did. A repository with no remote, or a detached HEAD with no --ref,
is refused; unpushed local work is pointed out.

BREAKING CHANGE: jobs need a remote and start from its copy of the ref.

Tests: <count> (the worktree-supersede and gc-via-job tests are gone)."
jj new
```

---

### Task 4: Delete worktrees and `gc`

Pure deletion. Nothing a job does creates a worktree any more.

**Files:**
- Delete: `src/gc.rs`, `tests/gc.rs`
- Modify: `src/lib.rs`, `src/cli.rs`, `src/main.rs`, `src/git.rs`,
  `src/paths.rs`, `CLAUDE.md`
- Test: `tests/git.rs`, `tests/paths.rs`, `tests/cli.rs`, `tests/delivery.rs`,
  `tests/support/mod.rs`

**Interfaces:**
- Produces: nothing new. Removes `gc`, `Command::Gc`,
  `git::{WorktreeStart, add_worktree, remove_worktree, prune_worktrees, delete_branch}`,
  `paths::{WORKTREE_ROOT_VAR, worktrees_root_given, worktrees_root, repo_worktrees_root, worktree_root, record_repository_for_worktrees, repository_owning_worktrees, repo_slug, stable_hash, REPOSITORY_MARKER}`,
  `JobPaths::worktree`.

- [ ] **Step 1: Delete the gc module and command**

```bash
rm src/gc.rs tests/gc.rs
```

Remove `pub mod gc;` from `src/lib.rs`; the `Gc` variant from `src/cli.rs`;
the `Command::Gc` arm, `remove_stale_worktrees`, and the `gc` import from
`src/main.rs`. `config::parse_duration` is still used by `job.rs`.

- [ ] **Step 2: Delete the worktree code**

From `src/git.rs`: `make_room_for_worktree`, `WorktreeStart`, `add_worktree`,
`remove_worktree`, `prune_worktrees`, `delete_branch`. Keep `branch_exists`
(tests use it) and `remote_exists` (`CLAUDE.md` cites it).

From `src/paths.rs`: everything listed under Interfaces. Update the module
doc to "Where a job's state lives on disk." `JobPaths::log`'s doc comment
becomes "Everything the job's commands printed, across every round — one
job, one log."

- [ ] **Step 3: Delete their tests**

From `tests/git.rs`: `a_worktree_leaves_the_original_tree_untouched`,
`an_existing_branch_can_be_checked_out_into_a_fresh_worktree`,
`checking_out_a_branch_that_is_already_in_a_worktree_is_refused`, and the
three `deleting_a_branch_…` tests. Tests that used a worktree only as a place
to commit (`diff_stat_…`, `seeded_files_…`, `a_commit_containing_only_seeded_files…`,
`a_secret_committed_by_the_agent…`) switch to a plain clone:
`git::clone_into` into a tempdir, then `git::check_out_new_branch`.

From `tests/paths.rs`: every test from `a_jobs_checkout_lives_below_its_worktree_directory`
and `worktrees_live_under_home_not_in_the_repo` through
`an_override_alone_is_enough_even_with_no_home`.

From `tests/cli.rs` and `tests/delivery.rs`: the `WORKTREE_ROOT_VAR` env in
`assembly()`, `worktree_root_for`, `job_worktrees`; rename
`discard_worktrees` to `discard_origin` (it now only removes `origin_for`).
Delete `gc_with_nothing_to_collect_says_so`.

From `tests/support/mod.rs`: `Harness::worktree_root`, the `Drop` impl, and
the `paths` import if unused.

- [ ] **Step 4: Update CLAUDE.md**

- "Prefer iterators to loops": change the attribution `(src/gc.rs's job_directories.)`
  to `(src/paths.rs's existing_job_ids has the same shape.)`.
- "Cost": replace "a `gc` run's stale worktrees" with "the frames one job
  prints".
- "Naming", filters bullet: replace with
  "**Filters name what they select:** `RepoConfig::settings_worth_flagging`,
  not `warnings` — it names which settings it picks out (`src/config.rs`)."
- "Invariants", last bullet: replace the worktree sentences with
  "Checkouts are scratch clones under the system temp directory, never
  inside the repository, and are deleted with the round that made them."
- "Verifying the stack": keep as is.

- [ ] **Step 5: Run everything**

Run: `just check`
Expected: green, with fewer tests than Task 3.

- [ ] **Step 6: Commit**

```bash
jj describe -m "refactor!: delete worktrees and gc

Jobs clone into scratch deleted with the round, so there are no worktrees to
place, mark, or collect. Removes src/gc.rs, assembly gc,
ASSEMBLY_WORKTREE_ROOT, the repo slug and marker, and git's worktree ops.

BREAKING CHANGE: assembly gc is gone. Worktrees left by F1 jobs can be
removed with \`git worktree prune\` after deleting ~/.assembly/wt.

Tests: <count>."
jj new
```

---

### Task 5: The host resolves a `JobPayload`

Everything a round needs becomes one serialisable value, built by the host
before the round starts. Still in-process; Task 6 moves it across a process
boundary.

**Files:**
- Create: `src/payload.rs`
- Modify: `src/lib.rs`, `src/provider.rs` (derive serde on `CommandSpec`),
  `src/job.rs`, `src/main.rs`
- Test: `tests/payload.rs`, `tests/support/mod.rs`, `tests/job.rs`,
  `tests/verify.rs`

**Interfaces:**
- Consumes: `git::PinnedRef`, `config::{RepoConfig, ConfigError, parse_duration}`,
  `provider::{CommandSpec, render_command}`, `workspace::job_branch_name`.
- Produces:

```rust
pub const PAYLOAD_VAR: &str = "ASSEMBLY_JOB";

pub struct JobPayload {
    pub job_id: u64,
    pub round: u32,
    pub remote_url: String,
    pub remote_name: String,
    pub start: PinnedRef,
    pub branch: String,
    pub command: CommandSpec,
    pub commit_message: String,
    pub verify: Option<String>,
    pub command_limit_secs: Option<u64>,
    pub copy: Vec<String>,
    pub seed_from: PathBuf,
    pub provision_toolchain: bool,
}

pub struct RoundRequest<'a> {
    pub job_id: u64,
    pub round: u32,
    pub prompt: &'a str,
    pub provider: &'a str,
    pub start: PinnedRef,
    pub remote_name: &'a str,
    pub remote_url: String,
    pub seed_from: &'a Path,
}

impl JobPayload {
    pub fn for_round(config: &RepoConfig, request: RoundRequest<'_>) -> anyhow::Result<JobPayload>;
}
pub fn revised_prompt(original: &str, feedback: &str) -> String;
pub async fn remote_to_clone(repo: &Path, remote: &str) -> anyhow::Result<String>;

// job.rs
pub async fn run_round(payload: &JobPayload, log: &mut EventLog, log_path: &Path, scratch_root: &Path, cancel: CancellationToken) -> anyhow::Result<JobOutcome>;
```

`RunOpts`, `JobSpec`, `Revision`, `run_job`, `revise_job` and `JobPlan` are
deleted — the payload is the plan.

- [ ] **Step 1: Write the failing tests**

`tests/payload.rs`:

```rust
use assembly_line::config::RepoConfig;
use assembly_line::git::PinnedRef;
use assembly_line::payload::{JobPayload, RoundRequest, revised_prompt};
use std::path::Path;

fn request(provider: &str) -> RoundRequest<'_> {
    RoundRequest {
        job_id: 7,
        round: 1,
        prompt: "add a README\n\nwith sections",
        provider,
        start: PinnedRef { name: "main".into(), sha: "abc123".into() },
        remote_name: "origin",
        remote_url: "https://example.com/o/r.git".into(),
        seed_from: Path::new("/repo"),
    }
}

fn config(body: &str) -> RepoConfig {
    RepoConfig::parse(body).unwrap()
}

const RUNNABLE: &str = "provider = \"fake\"\nverify = \"cargo test\"\nmax_duration = \"20m\"\ncopy = [\".env\"]\n\
[providers.fake]\ncmd = \"agent\"\nargs = [\"-p\", \"{prompt}\"]\n";

#[test]
fn a_payload_carries_everything_the_round_needs_already_resolved() {
    let payload = JobPayload::for_round(&config(RUNNABLE), request("fake")).unwrap();

    assert_eq!(payload.branch, "al/job-7");
    assert_eq!(payload.command.program, "agent");
    assert_eq!(payload.command.args, ["-p", "add a README\n\nwith sections"]);
    assert_eq!(payload.commit_message, "job 7: add a README");
    assert_eq!(payload.verify.as_deref(), Some("cargo test"));
    assert_eq!(payload.command_limit_secs, Some(20 * 60));
    assert_eq!(payload.copy, [".env"]);
    assert_eq!(payload.start.sha, "abc123");
    assert!(!payload.provision_toolchain, "the host opts a runner in, never the default");
}

#[test]
fn a_payload_round_trips_through_json() {
    let payload = JobPayload::for_round(&config(RUNNABLE), request("fake")).unwrap();
    let json = serde_json::to_string(&payload).unwrap();
    assert_eq!(serde_json::from_str::<JobPayload>(&json).unwrap(), payload);
}

#[test]
fn an_undeclared_provider_cannot_become_a_payload() {
    let err = JobPayload::for_round(&config(RUNNABLE), request("other")).unwrap_err();
    assert!(err.to_string().contains("'other'"), "{err}");
}

#[test]
fn a_revised_prompt_carries_the_original_and_the_feedback() {
    let prompt = revised_prompt("add auth", "use sessions");
    assert!(prompt.starts_with("add auth"));
    assert!(prompt.contains("use sessions"));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --test payload`
Expected: FAIL — `unresolved import assembly_line::payload`.

- [ ] **Step 3: Implement `src/payload.rs`**

Derive `serde::Serialize, serde::Deserialize` on `provider::CommandSpec`.
Move `commit_message` and `revised_prompt` out of `job.rs` (unchanged
bodies) and `remote_to_clone` out of `job.rs` (made `pub`).

```rust
//! Everything a round needs to run, resolved by the host before it starts.
//!
//! The payload is the whole plan. Whoever runs the round — this process
//! today, `job-exec` in a container from Task 6 on — executes exactly what
//! it says and reads no configuration of its own, so nothing inside the
//! boundary can influence the settings that govern it.

use crate::config::{ConfigError, RepoConfig, parse_duration};
use crate::git::{self, PinnedRef};
use crate::provider::{CommandSpec, render_command};
use crate::workspace::job_branch_name;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The environment variable a payload travels in: the one channel a child
/// process, `docker run` and a pod spec all share.
pub const PAYLOAD_VAR: &str = "ASSEMBLY_JOB";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobPayload {
    pub job_id: u64,
    pub round: u32,
    /// Where the round clones from and pushes to.
    pub remote_url: String,
    /// The host's name for that remote, which `JobBranchPublished` records.
    pub remote_name: String,
    /// The commit the round starts from — the base for round 1, the job's
    /// branch tip for a revise.
    pub start: PinnedRef,
    pub branch: String,
    pub command: CommandSpec,
    pub commit_message: String,
    pub verify: Option<String>,
    /// Cap on the agent, and again on `verify`, in whole seconds. See
    /// [`RepoConfig::max_duration`].
    pub command_limit_secs: Option<u64>,
    pub copy: Vec<String>,
    /// Where `copy` paths resolve from — the host's checkout, which only a
    /// runner sharing the host's filesystem can read.
    pub seed_from: PathBuf,
    /// Whether the round runs `mise install` before the agent. Set by
    /// runners whose jobs arrive in an image with no toolchain of the
    /// repository's own.
    pub provision_toolchain: bool,
}

/// What the host knows about a round before config has been applied to it.
#[derive(Debug, Clone)]
pub struct RoundRequest<'a> {
    pub job_id: u64,
    pub round: u32,
    pub prompt: &'a str,
    pub provider: &'a str,
    pub start: PinnedRef,
    pub remote_name: &'a str,
    pub remote_url: String,
    pub seed_from: &'a Path,
}

impl JobPayload {
    /// Apply the repository's config to a round.
    ///
    /// # Errors
    ///
    /// A [`ConfigError`] when the repository does not declare the provider,
    /// or declares a `max_duration` that is not a duration.
    pub fn for_round(config: &RepoConfig, request: RoundRequest<'_>) -> anyhow::Result<JobPayload> {
        let provider = config
            .providers
            .get(request.provider)
            .ok_or_else(|| ConfigError::UnknownProvider(request.provider.to_string()))?;
        let command_limit_secs = config
            .max_duration
            .as_deref()
            .map(parse_duration)
            .transpose()?
            .map(|limit| limit.as_secs());

        Ok(JobPayload {
            job_id: request.job_id,
            round: request.round,
            remote_url: request.remote_url,
            remote_name: request.remote_name.to_string(),
            start: request.start,
            branch: job_branch_name(request.job_id),
            command: render_command(provider, request.prompt),
            commit_message: commit_message(request.job_id, request.prompt),
            verify: config.verify.clone(),
            command_limit_secs,
            copy: config.copy.clone(),
            seed_from: request.seed_from.to_path_buf(),
            provision_toolchain: false,
        })
    }
}

// … commit_message, revised_prompt, remote_to_clone moved here verbatim …
```

- [ ] **Step 4: Make `job.rs` execute a payload**

`run_round` replaces `run_job` and `revise_job`:

```rust
/// Run one round of a job end to end: scratch clone, agent, commit, push,
/// `verify`, discard.
///
/// # Errors
///
/// Returns an error only if the round cannot be *administered* — the event
/// log cannot be appended to. An agent that fails, a clone that fails, and a
/// push that fails are all [`JobOutcome::Failed`], recorded in the log.
pub async fn run_round(
    payload: &JobPayload,
    log: &mut EventLog,
    log_path: &Path,
    scratch_root: &Path,
    cancel: CancellationToken,
) -> anyhow::Result<JobOutcome> {
    let timeout = payload.command_limit_secs.map(Duration::from_secs);

    log.append(EventKind::JobStarted { round: payload.round })?;

    let result = round_result(payload, log_path, scratch_root, timeout, cancel)
        .await
        .unwrap_or_else(|e| RoundResult::Failed {
            reason: e.to_string(),
            work: None,
        });

    record_completion(log, result)
}
```

`round_result`, `agent_work_on_branch` and `verify_verdict` take
`&JobPayload` where they took `&JobPlan`, reading `payload.remote_url`,
`payload.start`, `payload.branch`, `payload.seed_from`, `payload.copy`,
`payload.command`, `payload.commit_message`, `payload.remote_name` (for
`pushed_to`) and `payload.verify`. Delete `JobPlan`, `job_plan`, `RunOpts`,
`JobSpec`, `Revision`, `wall_clock_limit`, and the `ConfigError`/`paths`
imports.

- [ ] **Step 5: Build payloads in `main.rs`**

`PreparedJob` gains `remote_url: String` (from
`payload::remote_to_clone(&repo, DEFAULT_REMOTE)` in `prepare_job`, *before*
pinning, so a missing remote is reported as such). `runnable_config_and_provider`
additionally rejects an unparseable `max_duration` before allocation — it
already does via `reasons_it_cannot_run`.

`start_new_job`, after `allocate_job`:

```rust
    let payload = JobPayload::for_round(
        &config,
        RoundRequest {
            job_id: paths.id,
            round: 1,
            prompt: &prompt,
            provider: &provider,
            start,
            remote_name: DEFAULT_REMOTE,
            remote_url,
            seed_from: &repo,
        },
    )
    .map_err(|e| e.to_string())?;
    let outcome = run_round(&payload, &mut log, &paths.log(), &std::env::temp_dir(), cancel_on_ctrl_c())
        .await
        .map_err(|e| e.to_string())?;
```

where `cancel_on_ctrl_c` now returns the token it installs (fold
`machine_opts` into it; `machine_opts` is deleted). `revise_existing_job`
builds its payload with `round`, `prompt: &payload::revised_prompt(&meta.prompt, &feedback)`,
and `start: tip`.

- [ ] **Step 6: Update the harness and job tests**

`Harness::run_from(&start, prompt)` builds a payload with
`JobPayload::for_round` and calls `run_round(&payload, &mut log, &paths.log(), &self.scratch_root(), CancellationToken::new())`.
Tests that asserted an administrative error for an undeclared provider or an
unparseable `max_duration` now get it from `JobPayload::for_round` — have
`run_from` propagate that error with `?` so those tests are unchanged.
`a_repository_with_no_remote_cannot_run_a_job` now asserts on
`payload::remote_to_clone` directly:

```rust
#[tokio::test]
async fn a_repository_with_no_remote_cannot_run_a_job() {
    let h = Harness::new().await;
    git::run_allowing_failure(&h.repo, &["remote", "remove", "origin"]).await.unwrap();

    let err = payload::remote_to_clone(&h.repo, "origin").await.unwrap_err().to_string();
    assert!(err.contains("no 'origin' remote"), "{err}");
}
```

- [ ] **Step 7: Run everything**

Run: `just check`
Expected: green; count up by 4 from Task 4.

- [ ] **Step 8: Commit**

```bash
jj describe -m "refactor(payload): the host resolves a JobPayload

Everything a round needs — pinned start, remote URL, rendered provider
command, verify, timeouts, copy — becomes one serialisable value built
before the round starts. run_round executes it and reads no config of its
own. RunOpts, JobSpec, JobPlan and revise_job are gone.

Tests: <count>."
jj new
```

---

### Task 6: The job crosses a process boundary

`assembly job-exec` runs a payload from `ASSEMBLY_JOB` and prints frames. The
local runner spawns it; the collector turns its stream back into
`events.jsonl` and the log. From the user's side nothing changes.

**Files:**
- Create: `src/collect.rs`, `src/runner/mod.rs`, `src/runner/child.rs`, `src/runner/local.rs`
- Modify: `Cargo.toml` (add `nix = { version = "0.30", features = ["signal"] }`),
  `src/lib.rs`, `src/event.rs`, `src/exec.rs`, `src/job.rs`, `src/cli.rs`, `src/main.rs`
- Test: `tests/collect.rs`, `tests/exec.rs`, `tests/job.rs`, `tests/verify.rs`,
  `tests/support/mod.rs`, `tests/cli.rs`

**Interfaces:**
- Consumes: `frame::*`, `payload::{JobPayload, PAYLOAD_VAR}`.
- Produces:
  - `event::EventLog::append_collected(&mut self, &Event) -> io::Result<()>`
  - `exec::run_shell(cmd, cwd, output: &FrameWriter<W>, timeout, cancel)` and
    `exec::run_command(spec, cwd, output: &FrameWriter<W>, timeout, cancel)`,
    `W: Write + Send + 'static`
  - `job::run_round(payload, frames: &FrameWriter<W>, scratch_root: &Path, cancel) -> anyhow::Result<JobOutcome>`
  - `runner::Termination { Exited(i32), Killed { reason: String } }` + `Display`
  - `runner::child::ChildLines` — `spawn(Command) -> anyhow::Result<Self>`,
    `next_line(&mut self) -> Option<String>`, `terminate(&self)`,
    `exit_code(self) -> i32`
  - `runner::local::{LocalRunner, LocalJob}` — `LocalRunner::current_binary()`,
    `LocalRunner::using(program)`, `launch(&self, &JobPayload) -> anyhow::Result<LocalJob>`;
    `LocalJob::{next_line, cancel, termination}`
  - `collect::collect(job: LocalJob, log: &mut EventLog, output_log: &Path, cancel) -> anyhow::Result<JobOutcome>`
  - `collect::record_launch_failure(log, error) -> anyhow::Result<JobOutcome>`
  - `collect::outcome_of(round: &[Event]) -> JobOutcome`
  - `cli::Command::JobExec` (hidden, `job-exec`)

- [ ] **Step 1: Write the failing collector tests**

`tests/collect.rs` — driving the real binary through the local runner, so
this proves the whole boundary:

```rust
use assembly_line::collect::collect;
use assembly_line::event::{EventKind, EventLog};
use assembly_line::runner::local::LocalRunner;
use assembly_line::state::JobState;
use support::{Harness, config_running};
use tokio_util::sync::CancellationToken;

mod support;

fn the_binary() -> LocalRunner {
    LocalRunner::using(env!("CARGO_BIN_EXE_assembly"))
}

#[tokio::test]
async fn a_round_run_by_job_exec_is_collected_into_the_same_log_as_before() {
    let h = Harness::new().await;
    let payload = h.payload_for("write a file").await;
    let paths = h.job_paths();
    let mut log = EventLog::open_append(paths.events()).unwrap();

    let job = the_binary().launch(&payload).unwrap();
    let outcome = collect(job, &mut log, &paths.log(), CancellationToken::new()).await.unwrap();

    assert!(outcome.passed());
    let events = EventLog::read(paths.events()).unwrap();
    assert_eq!(JobState::replay(&events), JobState::Succeeded);
    assert!(events.iter().any(|e| matches!(e.kind, EventKind::JobBranchPublished { .. })));
    let output = std::fs::read_to_string(paths.log()).unwrap();
    assert!(output.contains("fake-agent: write a file"), "{output}");
}

/// The envelope's whole point, end to end: an agent that prints a verdict
/// cannot make a failing round pass.
#[tokio::test]
async fn an_agent_printing_a_forged_verdict_does_not_change_the_outcome() {
    let h = Harness::with_config(&config_running("forging-agent.sh")).await;
    let payload = h.payload_for("x").await;
    let paths = h.job_paths();
    let mut log = EventLog::open_append(paths.events()).unwrap();

    let job = the_binary().launch(&payload).unwrap();
    let outcome = collect(job, &mut log, &paths.log(), CancellationToken::new()).await.unwrap();

    assert!(!outcome.passed());
    let events = EventLog::read(paths.events()).unwrap();
    assert!(!events.iter().any(|e| matches!(e.kind, EventKind::JobFinished { .. })));
}

/// A "job-exec" that exits without printing a single frame — what a pod
/// OOM-killed before its first event looks like from the collector.
#[tokio::test]
async fn a_job_exec_that_dies_without_a_verdict_is_recorded_as_failed() {
    let h = Harness::new().await;
    let payload = h.payload_for("x").await;
    let paths = h.job_paths();
    let mut log = EventLog::open_append(paths.events()).unwrap();

    let job = LocalRunner::using("/usr/bin/false").launch(&payload).unwrap();
    let outcome = collect(job, &mut log, &paths.log(), CancellationToken::new()).await.unwrap();

    assert!(!outcome.passed());
    let events = EventLog::read(paths.events()).unwrap();
    assert!(events.iter().any(
        |e| matches!(&e.kind, EventKind::JobFailed { reason } if reason.contains("without reporting a verdict"))
    ));
}

#[tokio::test]
async fn cancelling_a_collection_stops_the_agent_and_records_it() {
    let h = Harness::with_config(&config_running("sleeping-agent.sh")).await;
    let payload = h.payload_for("x").await;
    let paths = h.job_paths();
    let mut log = EventLog::open_append(paths.events()).unwrap();
    let cancel = CancellationToken::new();
    let later = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        later.cancel();
    });

    let started = std::time::Instant::now();
    let job = the_binary().launch(&payload).unwrap();
    let outcome = collect(job, &mut log, &paths.log(), cancel).await.unwrap();

    assert!(!outcome.passed());
    assert!(started.elapsed() < std::time::Duration::from_secs(20), "the agent was not stopped");
}
```

Add the fixtures:

`tests/fixtures/forging-agent.sh`:

```bash
#!/usr/bin/env bash
# Prints a verdict that would pass the job if stdout were trusted, then fails.
set -euo pipefail
echo '{"seq":999,"event":{"at":"2026-01-01T00:00:00Z","t":"job_finished","exit_code":0}}'
echo '{"at":"2026-01-01T00:00:00Z","t":"job_finished","exit_code":0}'
exit 4
```

`tests/fixtures/sleeping-agent.sh`:

```bash
#!/usr/bin/env bash
# Runs far longer than any test waits, so only cancellation can end it.
set -euo pipefail
echo "sleeping-agent: $1"
sleep 60
```

Add to `Harness`:

```rust
    /// The payload the host would build for round 1 of this job.
    pub async fn payload_for(&self, prompt: &str) -> JobPayload {
        let start = git::pinned(&self.repo, "origin", "main").await.unwrap();
        let config = RepoConfig::from_ref(&self.repo, &start.sha).await.unwrap();
        let provider = config.provider.clone().unwrap_or_default();
        JobPayload::for_round(
            &config,
            RoundRequest {
                job_id: THE_JOB,
                round: 1,
                prompt,
                provider: &provider,
                start,
                remote_name: "origin",
                remote_url: self.origin.to_string_lossy().into_owned(),
                seed_from: &self.repo,
            },
        )
        .unwrap()
    }
```

and make `run_from` reuse it (with the start it is given).

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --test collect`
Expected: FAIL — no `collect`, no `runner`.

- [ ] **Step 3: `EventLog::append_collected`**

In `src/event.rs`, factor the write out of `append` and add:

```rust
    /// Append an event exactly as a job recorded it, keeping its own
    /// timestamp: the collector's copy of the job's log, not a new event.
    ///
    /// # Errors
    ///
    /// As [`EventLog::append`].
    pub fn append_collected(&mut self, event: &Event) -> io::Result<()> {
        self.write_line(event)
    }

    fn write_line(&mut self, event: &Event) -> io::Result<()> {
        let line = serde_json::to_string(event).map_err(io::Error::other)?;
        self.sink.write_all(line.as_bytes())?;
        self.sink.write_all(b"\n")?;
        self.sink.flush()
    }
```

`append` builds its `Event`, calls `write_line(&event)?`, returns the event.

- [ ] **Step 4: `exec.rs` frames its commands' output**

The file-log mode is replaced; nothing but `job-exec` runs commands now.

```rust
use crate::frame::FrameWriter;
use crate::payload::PAYLOAD_VAR;
use crate::provider::CommandSpec;
use std::io::Write;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

/// How long to keep forwarding output after a command exits. A command that
/// backgrounded a grandchild holding its stdout open would otherwise hold
/// the round forever.
const OUTPUT_DRAIN_GRACE: Duration = Duration::from_secs(2);
```

`run_shell` and `run_command` take `output: &FrameWriter<W>` in place of
`log_path` (`W: Write + Send + 'static`), and pass it through. `supervise`
becomes:

```rust
async fn supervise<W: Write + Send + 'static>(
    mut command: Command,
    described_as: &str,
    cwd: impl AsRef<Path>,
    output: &FrameWriter<W>,
    timeout: Option<Duration>,
    cancel: CancellationToken,
) -> anyhow::Result<ShellOutcome> {
    let mut child = command
        .current_dir(cwd.as_ref())
        // The payload names the job's plan; the agent has no use for it.
        .env_remove(PAYLOAD_VAR)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| anyhow::anyhow!("spawning `{described_as}`: {e}"))?;

    let forwarding = [
        forward_as_output(child.stdout.take(), output.clone()),
        forward_as_output(child.stderr.take(), output.clone()),
    ];

    let deadline = async {
        match timeout {
            Some(d) => tokio::time::sleep(d).await,
            None => std::future::pending::<()>().await,
        }
    };

    let outcome = tokio::select! {
        status = child.wait() => ShellOutcome::Exited(status?.code().unwrap_or(-1)),
        () = deadline => {
            let _ = child.kill().await;
            ShellOutcome::TimedOut
        }
        () = cancel.cancelled() => {
            let _ = child.kill().await;
            ShellOutcome::Cancelled
        }
    };

    // Every line the command printed is framed before the round's next
    // event, so the log reads in the order things happened. Two handles,
    // awaited in turn — a loop because each await is its own I/O.
    let drained = async {
        for handle in forwarding {
            let _ = handle.await;
        }
    };
    let _ = tokio::time::timeout(OUTPUT_DRAIN_GRACE, drained).await;
    Ok(outcome)
}

/// Forward each line `source` produces as an `output` frame, until it closes.
fn forward_as_output<W: Write + Send + 'static>(
    source: Option<impl AsyncRead + Unpin + Send + 'static>,
    output: FrameWriter<W>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let Some(source) = source else { return };
        let mut segments = BufReader::new(source).split(b'\n');
        while let Ok(Some(segment)) = segments.next_segment().await {
            if output.append_output(&String::from_utf8_lossy(&segment)).is_err() {
                return;
            }
        }
    })
}
```

Delete `open_log_for_append`.

Rewrite `tests/exec.rs` against frames. Replace each log-file read with:

```rust
use assembly_line::frame::{FrameWriter, Routed, StreamPosition};

fn printed(output: &FrameWriter<Vec<u8>>) -> Vec<String> {
    String::from_utf8(output.copy_of_sink())
        .unwrap()
        .lines()
        .filter_map(|line| match StreamPosition::default().route(line).1 {
            Routed::Output(text) => Some(text),
            _ => None,
        })
        .collect()
}
```

and each `&log` argument with `&output` where `let output = FrameWriter::new(Vec::new());`.
`appends_rather_than_truncating_across_runs` becomes
`two_commands_share_one_numbered_stream`: run two commands into the same
writer and assert both lines appear in order. Add:

```rust
#[tokio::test]
async fn a_command_never_sees_the_payload() {
    let tmp = tempfile::tempdir().unwrap();
    let output = FrameWriter::new(Vec::new());

    run_shell("echo \"payload=${ASSEMBLY_JOB:-absent}\"", tmp.path(), &output, None, CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(printed(&output), ["payload=absent"]);
}
```

(`cargo test` does not set `ASSEMBLY_JOB`, so this also passes without the
`env_remove`; its purpose is the `job-exec` path in `tests/collect.rs`, where
the variable *is* set. Keep it as documentation of the contract.)

- [ ] **Step 5: `job.rs` writes frames**

`run_round`'s signature becomes:

```rust
pub async fn run_round<W: Write + Send + 'static>(
    payload: &JobPayload,
    frames: &FrameWriter<W>,
    scratch_root: &Path,
    cancel: CancellationToken,
) -> anyhow::Result<JobOutcome>
```

Every `log.append(kind)` becomes `frames.append_event(kind)`; every
`log_path` argument to `run_command` / `run_shell` becomes `frames`.
`record_completion` takes `&FrameWriter<W>`.

- [ ] **Step 6: The runner module and `ChildLines`**

`src/runner/mod.rs`:

```rust
//! Where a job runs, and how its stream comes back.

pub mod child;
pub mod local;

/// Why a job's process stopped, as its runner can tell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Termination {
    Exited(i32),
    /// Stopped by something other than its own exit — out of memory,
    /// evicted, deleted. `reason` is the runner's own word for it.
    Killed { reason: String },
}

impl std::fmt::Display for Termination {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exited(code) => write!(f, "exit {code}"),
            Self::Killed { reason } => write!(f, "{reason}"),
        }
    }
}
```

`src/runner/child.rs`:

```rust
//! A child process's stdout and stderr, merged into one stream of lines.
//!
//! k8s merges the two anyway, so every runner treats them as one stream and
//! lets the frame parser tell `job-exec`'s frames from anything else.

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

#[derive(Debug)]
pub struct ChildLines {
    child: Child,
    lines: mpsc::Receiver<String>,
}

impl ChildLines {
    /// # Errors
    ///
    /// Returns an error if the program cannot be spawned.
    pub fn spawn(mut command: Command) -> anyhow::Result<Self> {
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| anyhow::anyhow!("spawning {:?}: {e}", command.as_std().get_program()))?;

        let (sender, lines) = mpsc::channel(256);
        forward_lines(child.stdout.take(), sender.clone());
        forward_lines(child.stderr.take(), sender);
        Ok(ChildLines { child, lines })
    }

    /// The next line on either stream, or `None` once both have closed.
    pub async fn next_line(&mut self) -> Option<String> {
        self.lines.recv().await
    }

    /// Ask the child to stop — SIGTERM, which `job-exec` answers by
    /// cancelling its agent and reporting the round.
    pub fn terminate(&self) {
        if let Some(pid) = self.child.id().and_then(|id| i32::try_from(id).ok()) {
            let _ = kill(Pid::from_raw(pid), Signal::SIGTERM);
        }
    }

    /// Wait for the child, returning its exit code, or -1 when a signal
    /// ended it.
    pub async fn exit_code(mut self) -> i32 {
        self.child
            .wait()
            .await
            .ok()
            .and_then(|status| status.code())
            .unwrap_or(-1)
    }
}

fn forward_lines(source: Option<impl AsyncRead + Unpin + Send + 'static>, sender: mpsc::Sender<String>) {
    let Some(source) = source else { return };
    tokio::spawn(async move {
        let mut segments = BufReader::new(source).split(b'\n');
        while let Ok(Some(segment)) = segments.next_segment().await {
            if sender.send(String::from_utf8_lossy(&segment).into_owned()).await.is_err() {
                return;
            }
        }
    });
}
```

`src/runner/local.rs`:

```rust
//! Running a job as a child of this process: no isolation, the host's own
//! toolchain and credentials.

use super::Termination;
use super::child::ChildLines;
use crate::payload::{JobPayload, PAYLOAD_VAR};
use std::path::PathBuf;
use tokio::process::Command;

#[derive(Debug, Clone)]
pub struct LocalRunner {
    program: PathBuf,
}

impl LocalRunner {
    /// This very binary, which is what `job-exec` is.
    ///
    /// # Errors
    ///
    /// Returns an error if the OS cannot say where this binary is.
    pub fn current_binary() -> std::io::Result<Self> {
        std::env::current_exe().map(Self::using)
    }

    /// A specific `assembly` binary — tests name the one cargo built.
    pub fn using(program: impl Into<PathBuf>) -> Self {
        LocalRunner {
            program: program.into(),
        }
    }

    /// # Errors
    ///
    /// Returns an error if the payload cannot be serialised or the binary
    /// cannot be spawned.
    pub fn launch(&self, payload: &JobPayload) -> anyhow::Result<LocalJob> {
        let mut command = Command::new(&self.program);
        command
            .arg("job-exec")
            .env(PAYLOAD_VAR, serde_json::to_string(payload)?);
        ChildLines::spawn(command).map(|lines| LocalJob { lines })
    }
}

#[derive(Debug)]
pub struct LocalJob {
    lines: ChildLines,
}

impl LocalJob {
    pub async fn next_line(&mut self) -> Option<String> {
        self.lines.next_line().await
    }

    pub async fn cancel(&mut self) {
        self.lines.terminate();
    }

    pub async fn termination(self) -> Termination {
        Termination::Exited(self.lines.exit_code().await)
    }
}
```

- [ ] **Step 7: The collector**

`src/collect.rs`:

```rust
//! The host's side of a job's stream: frames in, `events.jsonl` and the log
//! out. Runner-agnostic — it sees lines and a termination, nothing else.

use crate::event::{Event, EventKind, EventLog};
use crate::frame::{Routed, StreamPosition, verdict_missing_from};
use crate::job::JobOutcome;
use crate::runner::local::LocalJob;
use std::io::Write;
use std::path::Path;
use tokio_util::sync::CancellationToken;

/// Collect one round's stream until it ends, then settle its outcome.
///
/// A loop, not a fold: each line is I/O that must land before the next is
/// read, and cancellation arrives from outside mid-stream.
///
/// # Errors
///
/// Returns an error only if the log or the event log cannot be written. A
/// job that fails, or dies without saying how, is a [`JobOutcome::Failed`].
pub async fn collect(
    mut job: LocalJob,
    log: &mut EventLog,
    output_log: &Path,
    cancel: CancellationToken,
) -> anyhow::Result<JobOutcome> {
    let mut output = open_for_append(output_log)?;
    let mut position = StreamPosition::default();
    let mut collected: Vec<Event> = Vec::new();
    let mut cancelling = false;

    loop {
        let line = tokio::select! {
            line = job.next_line() => line,
            () = cancel.cancelled(), if !cancelling => {
                job.cancel().await;
                cancelling = true;
                continue;
            }
        };
        let Some(line) = line else { break };

        let (next, routed) = position.route(&line);
        position = next;
        match routed {
            Routed::Event { event, .. } => {
                log.append_collected(&event)?;
                collected.push(event);
            }
            Routed::Output(text) => writeln!(output, "{text}")?,
            Routed::AlreadyCollected => {}
        }
    }

    let termination = job.termination().await;
    if let Some(kind) = verdict_missing_from(&collected, &termination.to_string()) {
        collected.push(log.append(kind)?);
    }
    Ok(outcome_of(&collected))
}

/// A runner that could not start the job at all still leaves a record: the
/// round failed, and this is why.
///
/// # Errors
///
/// Returns an error if the event log cannot be written.
pub fn record_launch_failure(log: &mut EventLog, error: &anyhow::Error) -> anyhow::Result<JobOutcome> {
    log.append(EventKind::JobFailed {
        reason: format!("the runner could not start the job: {error}"),
    })?;
    Ok(JobOutcome::Failed)
}

/// What a round's events add up to: the last verdict in them.
#[must_use]
pub fn outcome_of(round: &[Event]) -> JobOutcome {
    round
        .iter()
        .rev()
        .find_map(|e| match e.kind {
            EventKind::JobFinished { .. } => Some(JobOutcome::Passed),
            EventKind::JobFailed { .. } => Some(JobOutcome::Failed),
            _ => None,
        })
        .unwrap_or(JobOutcome::Failed)
}

fn open_for_append(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new().create(true).append(true).open(path)
}
```

Add `pub mod collect;` and `pub mod runner;` to `src/lib.rs`.

- [ ] **Step 8: The `job-exec` subcommand**

In `src/cli.rs`:

```rust
    /// Run the round in ASSEMBLY_JOB and report it as frames on stdout.
    /// Started by a runner, never by hand.
    #[command(name = "job-exec", hide = true)]
    JobExec,
```

In `src/main.rs`, `install_tracing` adds `.with_writer(std::io::stderr)` —
stdout belongs to frames. Add the arm
`Command::JobExec => in_async_runtime(execute_payload_from_environment())`
and:

```rust
/// `job-exec`: read the payload, run the round, report it on stdout.
///
/// The exit code mirrors the round, but the collector decides from the
/// frames — the code only matters when the frames never said.
async fn execute_payload_from_environment() -> Result<ExitCode, String> {
    let payload: JobPayload = std::env::var(PAYLOAD_VAR)
        .map_err(|_| format!("{PAYLOAD_VAR} is not set — job-exec is started by a runner"))
        .and_then(|json| {
            serde_json::from_str(&json).map_err(|e| format!("{PAYLOAD_VAR} is not a job payload: {e}"))
        })?;

    let frames = FrameWriter::new(std::io::stdout());
    let cancel = CancellationToken::new();
    cancel_on_termination_signal(cancel.clone());

    run_round(&payload, &frames, &std::env::temp_dir(), cancel)
        .await
        .map(exit_code_for)
        .map_err(|e| e.to_string())
}

/// SIGTERM is how the local runner cancels, and SIGINT is Ctrl-C reaching
/// the whole foreground process group. Either one cancels the round, which
/// then still reports itself.
fn cancel_on_termination_signal(cancel: CancellationToken) {
    tokio::spawn(async move {
        let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        else {
            return;
        };
        tokio::select! {
            _ = terminate.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
        cancel.cancel();
    });
}
```

- [ ] **Step 9: `run` and `revise` go through the local runner**

In `start_new_job` and `revise_existing_job`, replace the in-process
`run_round` call with:

```rust
    let outcome = collect_round(&payload, &mut log, &paths.log(), cancel_on_ctrl_c())
        .await
        .map_err(|e| e.to_string())?;
```

```rust
/// Launch the round and collect it. A launch failure is a failed round, not
/// a usage error: the job directory already exists and must say what
/// became of it.
async fn collect_round(
    payload: &JobPayload,
    log: &mut EventLog,
    output_log: &Path,
    cancel: CancellationToken,
) -> anyhow::Result<JobOutcome> {
    match LocalRunner::current_binary()
        .map_err(anyhow::Error::from)
        .and_then(|runner| runner.launch(payload))
    {
        Ok(job) => collect(job, log, output_log, cancel).await,
        Err(e) => record_launch_failure(log, &e),
    }
}
```

`cancel_on_ctrl_c`'s message stays "interrupted — cancelling the running
agent".

- [ ] **Step 10: Update the job and verify tests**

`Harness::run_from` runs the round in-process against a
`FrameWriter<Vec<u8>>`, then routes the frames into `Outcome`:

```rust
        let frames = FrameWriter::new(Vec::new());
        let outcome = run_round(&payload, &frames, &self.scratch_root(), CancellationToken::new()).await?;
        let routed: Vec<Routed> = String::from_utf8(frames.copy_of_sink())
            .unwrap()
            .lines()
            .map(|line| StreamPosition::default().route(line).1)
            .collect();
        let events: Vec<Event> = routed.iter().filter_map(|r| match r {
            Routed::Event { event, .. } => Some(event.clone()),
            _ => None,
        }).collect();
        let output: String = routed.iter().filter_map(|r| match r {
            Routed::Output(text) => Some(format!("{text}\n")),
            _ => None,
        }).collect();
```

`Outcome.log: PathBuf` becomes `Outcome.output: String`. Every test reading
`std::fs::read_to_string(&outcome.log)` reads `outcome.output` instead
(`tests/job.rs`, `tests/verify.rs`).

- [ ] **Step 11: Run everything**

Run: `just check`
Expected: green, including the unchanged `tests/cli.rs` suite — it now runs
every job through `job-exec`.

- [ ] **Step 12: Commit**

```bash
jj describe -m "feat(job-exec): the job crosses a process boundary

assembly job-exec runs the payload in ASSEMBLY_JOB and prints frames on
stdout; its commands' output is framed as it happens and the payload is
withheld from them. The local runner spawns it and forwards its merged
stdout/stderr; the collector writes events.jsonl and the log from the
stream, dedupes by seq, and records a verdict for a round that ended
without one. run and revise go through it. Tracing moves to stderr.

Tests: <count>."
jj new
```

---

### Task 7: The `Runner` trait and the docker runner

The second implementor arrives, so the trait does too. Adds the runner
flags, host-chosen credentials, SSH→HTTPS rewriting, the `copy` refusal and
the `mise` cache volume.

**Files:**
- Create: `src/runner/docker.rs`
- Modify: `src/runner/mod.rs`, `src/runner/local.rs`, `src/collect.rs`,
  `src/payload.rs`, `src/exec.rs`, `src/workspace.rs`, `src/git.rs`,
  `src/job.rs`, `src/cli.rs`, `src/main.rs`
- Test: `tests/runner.rs`, `tests/docker_runner.rs`, `tests/payload.rs`, `tests/cli.rs`

**Interfaces:**
- Produces:

```rust
// runner/mod.rs
pub trait Runner {
    type Running: RunningJob + Send;
    /// Whether jobs run somewhere sharing nothing with the host — no
    /// filesystem, no toolchain, no git credentials.
    const RUNS_IN_A_CONTAINER: bool;
    fn reasons_it_cannot_run(&self) -> impl Future<Output = Vec<RunnerProblem>> + Send;
    fn launch(&self, payload: &JobPayload, secrets: &JobSecrets) -> impl Future<Output = anyhow::Result<Self::Running>> + Send;
}
pub trait RunningJob {
    fn next_line(&mut self) -> impl Future<Output = Option<String>> + Send;
    fn cancel(&mut self) -> impl Future<Output = ()> + Send;
    fn termination(self) -> impl Future<Output = Termination> + Send;
}
pub enum RunnerProblem { Unreachable { runner: &'static str, detail: String }, CannotCreate { resource: String, namespace: String }, CopyNeedsLocalRunner, MissingEnvironment(String) }
pub struct JobSecrets { vars: BTreeMap<String, String> }  // names(), vars()
impl JobSecrets { pub fn from_lookup(pass_env: &[String], lookup: impl Fn(&str) -> Option<String>) -> (JobSecrets, Vec<RunnerProblem>); pub fn from_host_environment(pass_env: &[String]) -> (JobSecrets, Vec<RunnerProblem>); }
pub fn reasons_a_container_cannot_run(copy: &[String]) -> Vec<RunnerProblem>;
pub fn published_image() -> String;

// payload.rs
pub const GIT_TOKEN_VAR: &str = "ASSEMBLY_GIT_TOKEN";
pub fn https_equivalent(url: &str) -> String;

// runner/docker.rs
pub struct DockerRunner { pub program: PathBuf, pub image: String }
pub const MISE_CACHE_VOLUME: &str = "assembly-mise-cache";
pub const MISE_DATA_DIR: &str = "/mise";
pub fn docker_run_args(image: &str, container: &str, env_names: &[&str]) -> Vec<String>;

// collect.rs
pub async fn collect<J: RunningJob>(job: J, ...) -> ...;

// cli.rs
pub enum RunnerKind { Local, Docker }   // K8s in Task 9
pub struct RunnerArgs { pub runner: RunnerKind, pub image: Option<String>, pub pass_env: Vec<String> }
```

- [ ] **Step 1: Write the failing pure tests**

`tests/runner.rs`:

```rust
use assembly_line::payload::{GIT_TOKEN_VAR, https_equivalent};
use assembly_line::runner::{JobSecrets, RunnerProblem, reasons_a_container_cannot_run};
use assembly_line::runner::docker::docker_run_args;

#[test]
fn ssh_remotes_become_the_https_url_a_token_can_authenticate() {
    assert_eq!(https_equivalent("git@github.com:o/r.git"), "https://github.com/o/r.git");
    assert_eq!(https_equivalent("ssh://git@github.com/o/r.git"), "https://github.com/o/r.git");
    assert_eq!(https_equivalent("ssh://git@github.com:22/o/r.git"), "https://github.com/o/r.git");
}

#[test]
fn urls_a_token_already_works_with_are_left_alone() {
    for url in ["https://github.com/o/r.git", "/tmp/origin.git", "file:///tmp/origin.git", "../origin"] {
        assert_eq!(https_equivalent(url), url);
    }
}

#[test]
fn a_container_always_receives_the_git_token_and_only_the_named_extras() {
    let host = |name: &str| match name {
        "ASSEMBLY_GIT_TOKEN" => Some("t0ken".to_string()),
        "ANTHROPIC_API_KEY" => Some("sk".to_string()),
        "AWS_SECRET_ACCESS_KEY" => Some("never".to_string()),
        _ => None,
    };

    let (secrets, problems) = JobSecrets::from_lookup(&["ANTHROPIC_API_KEY".into()], host);

    assert!(problems.is_empty(), "{problems:?}");
    // Sorted: `names` is a set.
    assert_eq!(
        secrets.names().into_iter().collect::<Vec<_>>(),
        ["ANTHROPIC_API_KEY", GIT_TOKEN_VAR]
    );
}

#[test]
fn every_missing_variable_is_reported_at_once() {
    let (_, problems) = JobSecrets::from_lookup(&["ANTHROPIC_API_KEY".into()], |_| None);
    assert_eq!(
        problems,
        [
            RunnerProblem::MissingEnvironment(GIT_TOKEN_VAR.into()),
            RunnerProblem::MissingEnvironment("ANTHROPIC_API_KEY".into()),
        ]
    );
}

#[test]
fn a_repository_that_declares_copy_cannot_run_in_a_container() {
    assert_eq!(reasons_a_container_cannot_run(&[".env".into()]), [RunnerProblem::CopyNeedsLocalRunner]);
    assert!(reasons_a_container_cannot_run(&[]).is_empty());
}

#[test]
fn every_runner_problem_says_what_to_do_about_it() {
    let problems = [
        RunnerProblem::Unreachable { runner: "docker", detail: "no daemon".into() },
        RunnerProblem::CannotCreate { resource: "jobs".into(), namespace: "factory".into() },
        RunnerProblem::CopyNeedsLocalRunner,
        RunnerProblem::MissingEnvironment("X".into()),
    ];
    for problem in problems {
        let message = problem.to_string();
        assert!(message.contains(" — "), "no remedy in: {message}");
    }
}

/// Values travel in the docker client's environment; the command line only
/// ever names them, so a secret never shows up in `ps`.
#[test]
fn docker_run_names_secrets_and_never_carries_their_values() {
    let args = docker_run_args("img:1", "al-1-1-abc", &["ASSEMBLY_JOB", "ASSEMBLY_GIT_TOKEN"]);
    let joined = args.join(" ");

    assert!(joined.contains("-e ASSEMBLY_JOB"), "{joined}");
    assert!(joined.contains("-e ASSEMBLY_GIT_TOKEN"), "{joined}");
    assert!(!joined.contains('='), "a value leaked onto the command line: {joined}");
    assert!(joined.contains("assembly-mise-cache:/mise"), "{joined}");
    assert!(joined.ends_with("img:1 assembly job-exec"), "{joined}");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --test runner`
Expected: FAIL — unresolved imports.

- [ ] **Step 3: Implement the trait, secrets and problems**

`src/runner/mod.rs` gains (keeping `Termination`):

```rust
pub mod docker;

use crate::payload::{GIT_TOKEN_VAR, JobPayload};
use std::collections::{BTreeMap, BTreeSet};

/// Where the image a container runner launches is published.
pub const PUBLISHED_IMAGE_REPOSITORY: &str = "ghcr.io/hmbill694/assembly-line";

/// The image whose `job-exec` matches this binary exactly, so the collector
/// and the job never disagree about the frame format.
#[must_use]
pub fn published_image() -> String {
    format!("{PUBLISHED_IMAGE_REPOSITORY}:{}", env!("CARGO_PKG_VERSION"))
}

/// One way of running `job-exec` somewhere.
pub trait Runner {
    type Running: RunningJob + Send;

    /// Whether jobs run somewhere sharing nothing with the host. Decides
    /// whether the payload provisions a toolchain, whether the remote URL
    /// must suit a token, whether `copy` can work, and whether the git
    /// credential has to be sent along.
    const RUNS_IN_A_CONTAINER: bool;

    /// Every reason this runner cannot launch a job right now, checked
    /// before a job directory is allocated.
    fn reasons_it_cannot_run(&self) -> impl Future<Output = Vec<RunnerProblem>> + Send;

    fn launch(
        &self,
        payload: &JobPayload,
        secrets: &JobSecrets,
    ) -> impl Future<Output = anyhow::Result<Self::Running>> + Send;
}

/// A launched job: its stream, a way to stop it, and why it stopped.
pub trait RunningJob {
    /// The next line of the job's merged output, or `None` at its end. A
    /// runner whose stream can drop reconnects inside this.
    fn next_line(&mut self) -> impl Future<Output = Option<String>> + Send;
    fn cancel(&mut self) -> impl Future<Output = ()> + Send;
    fn termination(self) -> impl Future<Output = Termination> + Send;
}

/// Something that stops a runner from launching a job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunnerProblem {
    Unreachable { runner: &'static str, detail: String },
    CannotCreate { resource: String, namespace: String },
    CopyNeedsLocalRunner,
    MissingEnvironment(String),
}

impl std::fmt::Display for RunnerProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable { runner, detail } => write!(
                f,
                "`{runner}` cannot be reached — install it or check its context: {detail}"
            ),
            Self::CannotCreate { resource, namespace } => write!(
                f,
                "cannot create {resource} in namespace '{namespace}' — grant the permission, \
                 or pass a --namespace where you have it"
            ),
            Self::CopyNeedsLocalRunner => write!(
                f,
                "this repository declares `copy`, which reads files from your checkout — a \
                 container has no access to it; use --runner local"
            ),
            Self::MissingEnvironment(name) => write!(
                f,
                "${name} is not set — export it, since the job's container receives it from \
                 your environment"
            ),
        }
    }
}

/// Environment a container job receives, by name. Read from the host once,
/// here, and only for names the host chose.
#[derive(Debug, Clone, Default)]
pub struct JobSecrets {
    vars: BTreeMap<String, String>,
}

impl JobSecrets {
    /// The git credential, always, plus each name in `pass_env`, looked up
    /// with `lookup`. Every name that has no value is a problem.
    pub fn from_lookup(
        pass_env: &[String],
        lookup: impl Fn(&str) -> Option<String>,
    ) -> (JobSecrets, Vec<RunnerProblem>) {
        let names: Vec<&str> = std::iter::once(GIT_TOKEN_VAR)
            .chain(pass_env.iter().map(String::as_str))
            .collect();
        let (found, missing): (Vec<_>, Vec<_>) = names
            .into_iter()
            .map(|name| (name, lookup(name)))
            .partition(|(_, value)| value.is_some());

        (
            JobSecrets {
                vars: found
                    .into_iter()
                    .filter_map(|(name, value)| Some((name.to_string(), value?)))
                    .collect(),
            },
            missing
                .into_iter()
                .map(|(name, _)| RunnerProblem::MissingEnvironment(name.to_string()))
                .collect(),
        )
    }

    pub fn from_host_environment(pass_env: &[String]) -> (JobSecrets, Vec<RunnerProblem>) {
        Self::from_lookup(pass_env, |name| std::env::var(name).ok())
    }

    #[must_use]
    pub fn names(&self) -> BTreeSet<String> {
        self.vars.keys().cloned().collect()
    }

    #[must_use]
    pub fn vars(&self) -> &BTreeMap<String, String> {
        &self.vars
    }
}

/// What a repository asks for that no container can give it.
#[must_use]
pub fn reasons_a_container_cannot_run(copy: &[String]) -> Vec<RunnerProblem> {
    (!copy.is_empty())
        .then_some(RunnerProblem::CopyNeedsLocalRunner)
        .into_iter()
        .collect()
}
```

In `src/payload.rs`:

```rust
/// The git credential a container job clones and pushes with. Never visible
/// to the agent — see [`crate::exec`].
pub const GIT_TOKEN_VAR: &str = "ASSEMBLY_GIT_TOKEN";

/// The HTTPS form of an SSH remote URL, which is what a token can
/// authenticate. Anything else — HTTPS already, a local path, `file://` —
/// comes back unchanged.
#[must_use]
pub fn https_equivalent(url: &str) -> String {
    let authority_and_path = match url.strip_prefix("ssh://") {
        Some(rest) => rest.split_once('/'),
        // scp-like `host:path`, which git recognises by a colon before any
        // slash. A local path has no such colon.
        None if !url.contains("://") => url
            .split_once(':')
            .filter(|(authority, _)| !authority.contains('/')),
        None => None,
    };

    match authority_and_path {
        Some((authority, path)) => {
            let host = authority.rsplit('@').next().unwrap_or(authority);
            let host = host.split(':').next().unwrap_or(host);
            format!("https://{host}/{path}")
        }
        None => url.to_string(),
    }
}
```

- [ ] **Step 4: Local implements the trait; the collector is generic**

`LocalRunner` implements `Runner` with `RUNS_IN_A_CONTAINER = false`,
`reasons_it_cannot_run` returning `async { Vec::new() }`, and `launch`
ignoring `secrets` (the child inherits the host's environment). `LocalJob`
implements `RunningJob` by moving its three inherent methods into the impl.
`collect` becomes `pub async fn collect<J: RunningJob>(mut job: J, …)`.
`tests/collect.rs` calls `the_binary().launch(&payload, &JobSecrets::default()).await`.

- [ ] **Step 5: `job-exec` uses the token and hides it from the agent**

In `src/git.rs`:

```rust
/// A credential helper answering with the token in
/// [`crate::payload::GIT_TOKEN_VAR`], read from the environment when git
/// asks — never written to disk.
pub const TOKEN_CREDENTIAL_HELPER: &str =
    "!f() { test \"$1\" = get && echo username=x-access-token && echo \"password=$ASSEMBLY_GIT_TOKEN\"; }; f";
```

`clone_into` gains `credential_helper: Option<&str>`: when `Some`, it runs
`git -c credential.helper=<helper> clone …` and then
`git config credential.helper <helper>` in the clone so the push uses it too.
`workspace::create` gains the same parameter and passes it through.
`job::run_round` passes
`std::env::var_os(GIT_TOKEN_VAR).is_some().then_some(git::TOKEN_CREDENTIAL_HELPER)`.
Update the Task 3 test calls to pass `None`.

In `exec::supervise`, add `.env_remove(GIT_TOKEN_VAR)` beside
`.env_remove(PAYLOAD_VAR)`, and extend `a_command_never_sees_the_payload` to
`a_command_never_sees_the_payload_or_the_git_token`, echoing both.

- [ ] **Step 6: Write the failing docker runner test**

`tests/docker_runner.rs` — a fake `docker` that runs `job-exec` for real
where a container would have started:

```rust
use assembly_line::collect::collect;
use assembly_line::event::{EventKind, EventLog};
use assembly_line::payload::GIT_TOKEN_VAR;
use assembly_line::runner::docker::DockerRunner;
use assembly_line::runner::{JobSecrets, Runner, RunnerProblem, RunningJob};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use support::Harness;
use tokio_util::sync::CancellationToken;

mod support;

/// Write an executable shell script called `name` into `dir`.
fn fake_cli(dir: &Path, name: &str, body: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/usr/bin/env bash\nset -euo pipefail\n{body}")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A `docker` that records its argv and, for `run`, executes `job-exec`
/// directly — the environment `docker run -e NAME` would forward is already
/// the environment this script inherits.
fn fake_docker(dir: &Path, argv_log: &Path, oom: bool) -> PathBuf {
    fake_cli(
        dir,
        "docker",
        &format!(
            "echo \"$*\" >> {log}\n\
             case \"$1\" in\n\
               run) exec {bin} job-exec ;;\n\
               version) echo 27.0.0 ;;\n\
               inspect) echo {oom} ;;\n\
               *) ;;\n\
             esac\n",
            log = argv_log.display(),
            bin = env!("CARGO_BIN_EXE_assembly"),
        ),
    )
}

fn token() -> JobSecrets {
    JobSecrets::from_lookup(&[], |name| (name == GIT_TOKEN_VAR).then(|| "t".to_string())).0
}

#[tokio::test]
async fn a_round_in_docker_is_launched_collected_and_cleaned_up() {
    let h = Harness::new().await;
    let fakes = h.scratch_root().with_file_name("fakes");
    let argv = fakes.join("argv");
    let runner = DockerRunner { program: fake_docker(&fakes, &argv, false), image: "img:1".into() };
    let payload = h.payload_for("write a file").await;
    let paths = h.job_paths();
    let mut log = EventLog::open_append(paths.events()).unwrap();

    let job = runner.launch(&payload, &token()).await.unwrap();
    let outcome = collect(job, &mut log, &paths.log(), CancellationToken::new()).await.unwrap();

    assert!(outcome.passed(), "{:?}", EventLog::read(paths.events()).unwrap());
    let calls = std::fs::read_to_string(&argv).unwrap();
    assert!(calls.contains("run --name al-1-1-"), "{calls}");
    assert!(calls.contains("-e ASSEMBLY_GIT_TOKEN"), "{calls}");
    assert!(calls.lines().any(|l| l.starts_with("rm -f al-1-1-")), "the container was left behind: {calls}");
}

#[tokio::test]
async fn an_oom_killed_container_is_reported_by_its_reason() {
    let h = Harness::with_config(&support::config_running("failing-agent.sh")).await;
    let fakes = h.scratch_root().with_file_name("fakes");
    let runner = DockerRunner { program: fake_docker(&fakes, &fakes.join("argv"), true), image: "img:1".into() };
    let mut job = runner.launch(&h.payload_for("x").await, &token()).await.unwrap();

    while job.next_line().await.is_some() {}
    let termination = job.termination().await;

    assert_eq!(termination.to_string(), "out of memory");
}

#[tokio::test]
async fn an_unreachable_docker_is_a_preflight_problem() {
    let tmp = tempfile::tempdir().unwrap();
    let runner = DockerRunner {
        program: fake_cli(tmp.path(), "docker", "echo 'Cannot connect to the Docker daemon' >&2\nexit 1\n"),
        image: "img:1".into(),
    };

    let problems = runner.reasons_it_cannot_run().await;
    assert!(matches!(problems.as_slice(), [RunnerProblem::Unreachable { runner: "docker", .. }]), "{problems:?}");
}
```

Note: `failing-agent.sh` exits 3, so the fake `docker run` exits non-zero and
the runner consults `inspect`, which says `true`.

- [ ] **Step 7: Run to verify it fails**

Run: `cargo test --test docker_runner`
Expected: FAIL — no `runner::docker`.

- [ ] **Step 8: Implement `src/runner/docker.rs`**

```rust
//! Running a job in a container, through the `docker` CLI.

use super::child::ChildLines;
use super::{JobSecrets, Runner, RunnerProblem, RunningJob, Termination};
use crate::payload::{JobPayload, PAYLOAD_VAR};
use std::path::PathBuf;
use tokio::process::Command;

/// The named volume `mise` installs toolchains into, shared by every job so
/// only the first job per toolchain version pays for it.
pub const MISE_CACHE_VOLUME: &str = "assembly-mise-cache";
/// Where the image keeps `mise`'s data, and so where the volume mounts.
pub const MISE_DATA_DIR: &str = "/mise";

#[derive(Debug, Clone)]
pub struct DockerRunner {
    /// `docker`, or a stand-in a test wrote.
    pub program: PathBuf,
    pub image: String,
}

impl DockerRunner {
    #[must_use]
    pub fn new(image: String) -> Self {
        DockerRunner { program: "docker".into(), image }
    }
}

/// A name unique to this round, so two repositories' job 1 never collide.
pub(crate) fn job_resource_name(payload: &JobPayload) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("al-{}-{}-{nanos:x}", payload.job_id, payload.round)
}

/// `docker run`'s arguments. Only *names* of environment variables appear:
/// `-e NAME` takes the value from the client's environment, so no secret is
/// ever visible on a command line.
///
/// No `--rm`: a container that exits non-zero is inspected first, to learn
/// whether it was OOM-killed, and removed afterwards.
#[must_use]
pub fn docker_run_args(image: &str, container: &str, env_names: &[&str]) -> Vec<String> {
    ["run", "--name", container]
        .into_iter()
        .map(String::from)
        .chain(env_names.iter().flat_map(|name| ["-e".to_string(), (*name).to_string()]))
        .chain([
            "-v".to_string(),
            format!("{MISE_CACHE_VOLUME}:{MISE_DATA_DIR}"),
            image.to_string(),
            "assembly".to_string(),
            "job-exec".to_string(),
        ])
        .collect()
}

impl Runner for DockerRunner {
    type Running = DockerJob;
    const RUNS_IN_A_CONTAINER: bool = true;

    async fn reasons_it_cannot_run(&self) -> Vec<RunnerProblem> {
        match Command::new(&self.program).args(["version", "--format", "{{.Server.Version}}"]).output().await {
            Ok(out) if out.status.success() => Vec::new(),
            Ok(out) => vec![RunnerProblem::Unreachable {
                runner: "docker",
                detail: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            }],
            Err(e) => vec![RunnerProblem::Unreachable { runner: "docker", detail: e.to_string() }],
        }
    }

    async fn launch(&self, payload: &JobPayload, secrets: &JobSecrets) -> anyhow::Result<DockerJob> {
        let container = job_resource_name(payload);
        let names = secrets.names();
        let env_names: Vec<&str> = std::iter::once(PAYLOAD_VAR)
            .chain(names.iter().map(String::as_str))
            .collect();

        let mut command = Command::new(&self.program);
        command
            .args(docker_run_args(&self.image, &container, &env_names))
            .env(PAYLOAD_VAR, serde_json::to_string(payload)?)
            .envs(secrets.vars());

        ChildLines::spawn(command).map(|lines| DockerJob {
            lines,
            container,
            program: self.program.clone(),
        })
    }
}
```

`impl Runner` uses `async fn` in the *impl* — allowed and not linted; only
the trait declaration must use `impl Future`. If the compiler asks for
`Send` bounds on the returned futures, the fields used here are all `Send`.

```rust
#[derive(Debug)]
pub struct DockerJob {
    lines: ChildLines,
    container: String,
    program: PathBuf,
}

impl DockerJob {
    async fn docker(&self, args: &[&str]) -> Option<String> {
        Command::new(&self.program)
            .args(args)
            .output()
            .await
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}

impl RunningJob for DockerJob {
    async fn next_line(&mut self) -> Option<String> {
        self.lines.next_line().await
    }

    /// Killing the `docker` client does not stop the container; removing it
    /// does, and the client exits with it.
    async fn cancel(&mut self) {
        let _ = self.docker(&["rm", "-f", &self.container]).await;
    }

    async fn termination(self) -> Termination {
        let program = self.program.clone();
        let container = self.container.clone();
        let code = self.lines.exit_code().await;
        let oom_killed = match code {
            0 => false,
            _ => DockerJob::inspect_oom(&program, &container).await,
        };
        let _ = Command::new(&program).args(["rm", "-f", &container]).output().await;

        match oom_killed {
            true => Termination::Killed { reason: "out of memory".into() },
            false => Termination::Exited(code),
        }
    }
}

impl DockerJob {
    async fn inspect_oom(program: &std::path::Path, container: &str) -> bool {
        Command::new(program)
            .args(["inspect", "--format", "{{.State.OOMKilled}}", container])
            .output()
            .await
            .ok()
            .is_some_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "true")
    }
}
```

- [ ] **Step 9: CLI flags and runner dispatch**

In `src/cli.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum RunnerKind {
    /// A child process on this machine, with its toolchain and credentials
    Local,
    /// A container, through the `docker` CLI
    Docker,
}

/// Where a round runs. Shared by `run` and `revise`: a revise is a new job
/// cut from the branch, so it may run somewhere the first round did not.
#[derive(Debug, clap::Args)]
pub struct RunnerArgs {
    #[arg(long, value_enum, default_value_t = RunnerKind::Local)]
    pub runner: RunnerKind,
    /// The job image. Defaults to the published image at this version.
    #[arg(long)]
    pub image: Option<String>,
    /// Pass this variable from your environment into the job's container.
    /// Repeatable. ASSEMBLY_GIT_TOKEN is always passed.
    #[arg(long = "pass-env", value_name = "NAME")]
    pub pass_env: Vec<String>,
}
```

and `#[command(flatten)] runner: RunnerArgs` on both `Run` and `Revise`.

In `src/main.rs`, one dispatch point turns flags into a concrete runner and
calls generic code:

```rust
/// What `run` and `revise` do, independent of where the round runs.
enum Work {
    Start { prompt: Option<String>, prompt_file: Option<PathBuf>, repo: Option<PathBuf>, base_ref: Option<String>, provider: Option<String> },
    Revise { job_id: u64, feedback: String, repo: Option<PathBuf> },
}

async fn run_work_on_chosen_runner(args: RunnerArgs, work: Work) -> Result<ExitCode, String> {
    let image = args.image.clone().unwrap_or_else(runner::published_image);
    match args.runner {
        RunnerKind::Local if args.image.is_some() || !args.pass_env.is_empty() => Err(
            "--image and --pass-env apply to container runners; the local runner uses your \
             machine as it is"
                .into(),
        ),
        RunnerKind::Local => run_work(
            &LocalRunner::current_binary().map_err(|e| e.to_string())?,
            &[],
            work,
        )
        .await,
        RunnerKind::Docker => run_work(&DockerRunner::new(image), &args.pass_env, work).await,
    }
}

async fn run_work<R: Runner>(runner: &R, pass_env: &[String], work: Work) -> Result<ExitCode, String> {
    match work {
        Work::Start { .. } => start_new_job(runner, pass_env, …).await,
        Work::Revise { .. } => revise_existing_job(runner, pass_env, …).await,
    }
}
```

`start_new_job` and `revise_existing_job` become generic over `R: Runner`,
and gain a preflight between config and allocation:

```rust
/// Every reason the chosen runner cannot run this repository's job, printed
/// together, before anything is allocated.
async fn runnable_secrets<R: Runner>(
    runner: &R,
    config: &RepoConfig,
    pass_env: &[String],
) -> Result<JobSecrets, String> {
    let (secrets, missing) = match R::RUNS_IN_A_CONTAINER {
        true => JobSecrets::from_host_environment(pass_env),
        false => (JobSecrets::default(), Vec::new()),
    };
    let container = match R::RUNS_IN_A_CONTAINER {
        true => reasons_a_container_cannot_run(&config.copy),
        false => Vec::new(),
    };
    let problems: Vec<RunnerProblem> = runner
        .reasons_it_cannot_run()
        .await
        .into_iter()
        .chain(container)
        .chain(missing)
        .collect();

    match problems.as_slice() {
        [] => Ok(secrets),
        problems => {
            problems.iter().for_each(|p| eprintln!("error: {p}"));
            Err("the job cannot run on this runner".into())
        }
    }
}
```

When building the payload:

```rust
    remote_url: match R::RUNS_IN_A_CONTAINER {
        true => payload::https_equivalent(&remote_url),
        false => remote_url,
    },
```

and after `JobPayload::for_round`, set
`payload.provision_toolchain = R::RUNS_IN_A_CONTAINER` (bind `let payload =
JobPayload { provision_toolchain: R::RUNS_IN_A_CONTAINER, ..for_round(…)? }`).
`collect_round` becomes generic:

```rust
async fn collect_round<R: Runner>(
    runner: &R,
    payload: &JobPayload,
    secrets: &JobSecrets,
    log: &mut EventLog,
    output_log: &Path,
    cancel: CancellationToken,
) -> anyhow::Result<JobOutcome> {
    match runner.launch(payload, secrets).await {
        Ok(job) => collect(job, log, output_log, cancel).await,
        Err(e) => record_launch_failure(log, &e),
    }
}
```

- [ ] **Step 10: CLI tests for the new surface**

Append to `tests/cli.rs`:

```rust
#[tokio::test]
async fn container_flags_are_refused_for_the_local_runner() {
    let tmp = repo_running("fake-agent.sh").await;
    assembly(&tmp)
        .args(["run", "--prompt", "x", "--pass-env", "ANTHROPIC_API_KEY"])
        .assert()
        .code(2)
        .stderr(contains("container runners"));
    discard_origin(&tmp);
}

/// The docker runner through the real binary, with a fake `docker` first on
/// PATH. Every preflight problem is reported at once, and nothing is
/// allocated.
#[tokio::test]
async fn docker_preflight_reports_every_problem_before_allocating() {
    let tmp = repo_running_with_copy("fake-agent.sh").await;
    let fakes = tempfile::tempdir().unwrap();
    // A docker that cannot reach its daemon.
    write_fake(fakes.path(), "docker", "echo 'no daemon' >&2\nexit 1\n");

    assembly(&tmp)
        .env("PATH", format!("{}:{}", fakes.path().display(), std::env::var("PATH").unwrap()))
        .env_remove("ASSEMBLY_GIT_TOKEN")
        .args(["run", "--prompt", "x", "--runner", "docker"])
        .assert()
        .code(2)
        .stderr(contains("`docker` cannot be reached"))
        .stderr(contains("declares `copy`"))
        .stderr(contains("$ASSEMBLY_GIT_TOKEN is not set"));

    assert!(!tmp.path().join(".assembly/jobs/1").exists());
    discard_origin(&tmp);
}
```

with `repo_running_with_copy` committing a config that also declares
`copy = ["local.env"]` and a `write_fake(dir, name, body)` helper identical
to `tests/docker_runner.rs`'s `fake_cli`. (Move `fake_cli` into
`tests/support/mod.rs` as `pub fn fake_cli` and use it from both files.)

- [ ] **Step 11: Run everything**

Run: `just check`
Expected: green.

- [ ] **Step 12: Commit**

```bash
jj describe -m "feat(runner): the Runner trait, and a docker runner behind it

A second implementor earns the trait: Runner launches job-exec somewhere and
hands back a RunningJob — its lines, a cancel, and why it stopped. Docker
drives the CLI: -e names only, a shared mise cache volume, rm -f to cancel,
inspect for OOM. --runner, --image and --pass-env choose and configure it;
container runs are preflighted (daemon reachable, no copy, every named
variable set) before anything is allocated. ASSEMBLY_GIT_TOKEN feeds a git
credential helper and is withheld from the agent; SSH remotes are rewritten
to HTTPS for it.

Tests: <count>."
jj new
```

---

### Task 8: Container rounds provision their toolchain

**Files:**
- Modify: `src/job.rs`
- Test: `tests/provisioning.rs`

**Interfaces:**
- Produces: `job::PROVISIONING_LIMIT: Duration` (15 minutes). No signature
  changes: `run_round` reads `payload.provision_toolchain`.

- [ ] **Step 1: Write the failing tests**

`tests/provisioning.rs` — runs `job-exec` directly, with a fake `mise` on
its `PATH`:

```rust
use assembly_line::event::EventKind;
use assembly_line::frame::{Routed, StreamPosition};
use assembly_line::payload::PAYLOAD_VAR;
use assert_cmd::Command;
use support::{Harness, fake_cli};

mod support;

/// Run `job-exec` on `payload` with `fakes` first on PATH; return the
/// routed frames.
fn job_exec(payload: &assembly_line::payload::JobPayload, fakes: &std::path::Path, tmp: &std::path::Path) -> Vec<Routed> {
    let out = Command::cargo_bin("assembly")
        .unwrap()
        .arg("job-exec")
        .env(PAYLOAD_VAR, serde_json::to_string(payload).unwrap())
        .env("PATH", format!("{}:{}", fakes.display(), std::env::var("PATH").unwrap()))
        .env("TMPDIR", tmp)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|line| StreamPosition::default().route(line).1)
        .collect()
}

fn outputs(routed: &[Routed]) -> String {
    routed.iter().filter_map(|r| match r { Routed::Output(t) => Some(format!("{t}\n")), _ => None }).collect()
}

#[tokio::test]
async fn a_container_round_installs_the_toolchain_before_the_agent_runs() {
    let h = Harness::new().await;
    let fakes = h.scratch_root().with_file_name("fakes");
    fake_cli(&fakes, "mise", "echo \"mise $*\"\n");
    let payload = assembly_line::payload::JobPayload { provision_toolchain: true, ..h.payload_for("x").await };

    let routed = job_exec(&payload, &fakes, &h.scratch_root());
    let printed = outputs(&routed);

    let trusted = printed.find("mise trust").expect(&printed);
    let installed = printed.find("mise install").expect(&printed);
    let agent = printed.find("fake-agent: x").expect(&printed);
    assert!(trusted < installed && installed < agent, "{printed}");
}

#[tokio::test]
async fn a_failed_install_fails_the_round_before_the_agent_runs() {
    let h = Harness::new().await;
    let fakes = h.scratch_root().with_file_name("fakes");
    fake_cli(&fakes, "mise", "case \"$1\" in install) exit 1 ;; esac\n");
    let payload = assembly_line::payload::JobPayload { provision_toolchain: true, ..h.payload_for("x").await };

    let routed = job_exec(&payload, &fakes, &h.scratch_root());

    assert!(!outputs(&routed).contains("fake-agent"), "the agent ran on a toolchain that failed to install");
    assert!(routed.iter().any(|r| matches!(r,
        Routed::Event { event, .. } if matches!(&event.kind, EventKind::JobFailed { reason } if reason.contains("provisioning"))
    )));
}

#[tokio::test]
async fn a_local_round_never_touches_mise() {
    let h = Harness::new().await;
    let fakes = h.scratch_root().with_file_name("fakes");
    fake_cli(&fakes, "mise", "echo MISE-WAS-CALLED\n");

    let routed = job_exec(&h.payload_for("x").await, &fakes, &h.scratch_root());

    assert!(!outputs(&routed).contains("MISE-WAS-CALLED"));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --test provisioning`
Expected: FAIL — the first two tests (nothing calls `mise`).

- [ ] **Step 3: Provision in `job.rs`**

```rust
/// How long cloning and provisioning may take together. Not
/// `max_duration`: that is the repository's statement about its own
/// commands, and a cold toolchain install should not eat the agent's budget.
pub const PROVISIONING_LIMIT: Duration = Duration::from_secs(15 * 60);

/// `mise trust` first: `mise` refuses to act on a `mise.toml` in a directory
/// it has not been told to trust, and a fresh clone is exactly that.
const PROVISIONING_STEPS: [&[&str]; 2] = [&["trust", "--all", "--yes"], &["install", "--yes"]];

/// Install the repository's toolchain in the clone, when the payload asks.
async fn provision_toolchain<W: Write + Send + 'static>(
    payload: &JobPayload,
    ws: &JobWorkspace,
    frames: &FrameWriter<W>,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    if !payload.provision_toolchain {
        return Ok(());
    }
    // A loop: each step is sequential I/O, and a failure ends it.
    for args in PROVISIONING_STEPS {
        let spec = CommandSpec {
            program: "mise".into(),
            args: args.iter().map(|a| (*a).to_string()).collect(),
        };
        let outcome = run_command(&spec, ws.path(), frames, None, cancel.clone()).await?;
        if let Some(reason) = outcome.failure_reason() {
            anyhow::bail!("provisioning the toolchain failed: `mise {}` {reason}", args.join(" "));
        }
    }
    Ok(())
}
```

In `round_result`, wrap creation and provisioning together:

```rust
    let ws = tokio::time::timeout(PROVISIONING_LIMIT, async {
        let ws = workspace::create(…).await?;
        provision_toolchain(payload, &ws, frames, cancel.clone()).await?;
        anyhow::Ok(ws)
    })
    .await
    .map_err(|_| anyhow::anyhow!("provisioning timed out after {}", humantime::format_duration(PROVISIONING_LIMIT)))??;
```

A provisioning failure is an `Err` from `round_result`, which `run_round`
already records as `JobFailed` with no work — the agent never ran.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test --test provisioning` then `just check`.
Expected: green.

- [ ] **Step 5: Commit**

```bash
jj describe -m "feat(job-exec): container rounds provision their toolchain with mise

When the payload asks — container runners always do — job-exec runs
mise trust and mise install in the clone before the agent, so the image
needs nothing of the repository's own. Clone and provisioning share a 15m
cap outside max_duration. Local rounds never touch mise.

Tests: <count>."
jj new
```

---

### Task 9: The k8s runner

**Files:**
- Create: `src/runner/kubernetes.rs`
- Modify: `src/runner/mod.rs`, `src/cli.rs`, `src/main.rs`
- Test: `tests/kubernetes_runner.rs`, `tests/cli.rs`

**Interfaces:**
- Produces:

```rust
pub struct KubernetesRunner {
    pub program: PathBuf,
    pub image: String,
    pub namespace: String,
    pub context: Option<String>,
    pub scheduling_deadline: Duration,
    pub poll_interval: Duration,
}
impl KubernetesRunner { pub fn new(image: String, namespace: String, context: Option<String>) -> Self; }
pub const SCHEDULING_DEADLINE: Duration;  // 10 minutes
pub enum PodProgress { Gone, Waiting { pod: String, reason: String }, Running { pod: String }, Finished { pod: String, exit_code: i32, reason: Option<String> } }
pub fn pod_progress(pods: &serde_json::Value) -> PodProgress;
pub fn job_manifest(name: &str, image: &str, active_deadline_secs: Option<u64>) -> serde_json::Value;
pub fn secret_manifest(name: &str, job_uid: &str, vars: &BTreeMap<String, String>) -> serde_json::Value;
pub fn active_deadline_secs(payload: &JobPayload) -> Option<u64>;
pub fn split_timestamp(line: &str) -> (Option<&str>, &str);
// cli.rs
RunnerKind::K8s; RunnerArgs { …, namespace: Option<String>, context: Option<String> }
```

- [ ] **Step 1: Write the failing pure tests**

`tests/kubernetes_runner.rs` (pure half):

```rust
use assembly_line::runner::kubernetes::{
    PodProgress, active_deadline_secs, job_manifest, pod_progress, secret_manifest, split_timestamp,
};
use serde_json::json;
use std::collections::BTreeMap;

#[test]
fn a_job_manifest_never_retries_and_reads_its_environment_from_its_secret() {
    let job = job_manifest("al-1-1-abc", "img:1", Some(4200));

    assert_eq!(job["kind"], "Job");
    assert_eq!(job["spec"]["backoffLimit"], 0);
    assert_eq!(job["spec"]["activeDeadlineSeconds"], 4200);
    assert_eq!(job["spec"]["template"]["spec"]["restartPolicy"], "Never");
    let container = &job["spec"]["template"]["spec"]["containers"][0];
    assert_eq!(container["image"], "img:1");
    assert_eq!(container["command"], json!(["assembly", "job-exec"]));
    assert_eq!(container["envFrom"][0]["secretRef"]["name"], "al-1-1-abc");
}

#[test]
fn a_job_with_no_max_duration_has_no_active_deadline() {
    assert!(job_manifest("n", "i", None)["spec"].get("activeDeadlineSeconds").is_none());
}

/// Owned by the Job, so deleting the Job — on completion or cancel —
/// deletes the credentials with it.
#[test]
fn a_secret_is_owned_by_its_job_and_carries_the_payload() {
    let vars = BTreeMap::from([("ASSEMBLY_JOB".to_string(), "{}".to_string())]);
    let secret = secret_manifest("al-1-1-abc", "uid-9", &vars);

    assert_eq!(secret["kind"], "Secret");
    assert_eq!(secret["metadata"]["ownerReferences"][0]["uid"], "uid-9");
    assert_eq!(secret["metadata"]["ownerReferences"][0]["kind"], "Job");
    assert_eq!(secret["stringData"]["ASSEMBLY_JOB"], "{}");
}

#[test]
fn the_backstop_deadline_is_twice_the_command_limit_plus_half_an_hour() {
    // command_limit_secs = 20m
    let payload = support_payload(Some(1200));
    assert_eq!(active_deadline_secs(&payload), Some(2 * 1200 + 1800));
    assert_eq!(active_deadline_secs(&support_payload(None)), None);
}

fn pods(pod: serde_json::Value) -> serde_json::Value {
    json!({ "items": [pod] })
}

#[test]
fn pod_progress_reads_waiting_running_and_finished_pods() {
    assert_eq!(pod_progress(&json!({ "items": [] })), PodProgress::Gone);
    assert_eq!(
        pod_progress(&pods(json!({
            "metadata": { "name": "p" },
            "status": { "phase": "Pending", "containerStatuses": [{ "state": { "waiting": { "reason": "ImagePullBackOff" } } }] }
        }))),
        PodProgress::Waiting { pod: "p".into(), reason: "ImagePullBackOff".into() }
    );
    assert_eq!(
        pod_progress(&pods(json!({ "metadata": { "name": "p" }, "status": { "phase": "Pending" } }))),
        PodProgress::Waiting { pod: "p".into(), reason: "Pending".into() }
    );
    assert_eq!(
        pod_progress(&pods(json!({ "metadata": { "name": "p" }, "status": { "phase": "Running", "containerStatuses": [{ "state": { "running": {} } }] } }))),
        PodProgress::Running { pod: "p".into() }
    );
    assert_eq!(
        pod_progress(&pods(json!({
            "metadata": { "name": "p" },
            "status": { "phase": "Failed", "containerStatuses": [{ "state": { "terminated": { "exitCode": 137, "reason": "OOMKilled" } } }] }
        }))),
        PodProgress::Finished { pod: "p".into(), exit_code: 137, reason: Some("OOMKilled".into()) }
    );
    assert_eq!(
        pod_progress(&pods(json!({
            "metadata": { "name": "p" },
            "status": { "phase": "Succeeded", "containerStatuses": [{ "state": { "terminated": { "exitCode": 0, "reason": "Completed" } } }] }
        }))),
        PodProgress::Finished { pod: "p".into(), exit_code: 0, reason: None }
    );
}

#[test]
fn a_timestamped_log_line_splits_into_its_timestamp_and_its_text() {
    assert_eq!(
        split_timestamp("2026-09-21T10:00:00.123456789Z {\"seq\":1}"),
        (Some("2026-09-21T10:00:00.123456789Z"), "{\"seq\":1}")
    );
    assert_eq!(split_timestamp("no timestamp here"), (None, "no timestamp here"));
}
```

`support_payload(limit)` is a local helper building a `JobPayload` literal
with `command_limit_secs: limit` and placeholder strings for every other
field.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --test kubernetes_runner`
Expected: FAIL — no `runner::kubernetes`.

- [ ] **Step 3: Implement the pure half of `src/runner/kubernetes.rs`**

```rust
//! Running a job as a k8s Job, through the `kubectl` CLI.

use super::child::ChildLines;
use super::docker::job_resource_name;
use super::{JobSecrets, Runner, RunnerProblem, RunningJob, Termination};
use crate::payload::{JobPayload, PAYLOAD_VAR};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// How long a pod may sit unscheduled or unpulled before the job fails.
/// `Pending` forever is the likeliest k8s failure there is.
pub const SCHEDULING_DEADLINE: Duration = Duration::from_secs(10 * 60);
const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// How long a finished Job lingers for inspection if its own cleanup fails.
const TTL_AFTER_FINISHED_SECS: u64 = 3600;
/// Covers clone, provisioning and push on top of two command limits.
const BACKSTOP_ALLOWANCE_SECS: u64 = 30 * 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PodProgress {
    /// No pod: never created, or deleted.
    Gone,
    Waiting { pod: String, reason: String },
    Running { pod: String },
    Finished { pod: String, exit_code: i32, reason: Option<String> },
}

/// Where the Job's pod stands, read from `kubectl get pods -o json`.
#[must_use]
pub fn pod_progress(pods: &Value) -> PodProgress {
    let Some(pod) = pods.pointer("/items/0") else {
        return PodProgress::Gone;
    };
    let name = pod.pointer("/metadata/name").and_then(Value::as_str).unwrap_or_default().to_string();
    let phase = pod.pointer("/status/phase").and_then(Value::as_str).unwrap_or("Pending");
    let state = pod.pointer("/status/containerStatuses/0/state");
    let terminated = state.and_then(|s| s.get("terminated"));
    let waiting = state.and_then(|s| s.pointer("/waiting/reason")).and_then(Value::as_str);

    match (phase, terminated, waiting) {
        (_, Some(t), _) => PodProgress::Finished {
            pod: name,
            exit_code: t.get("exitCode").and_then(Value::as_i64).and_then(|c| i32::try_from(c).ok()).unwrap_or(-1),
            // "Completed" and "Error" only restate the exit code.
            reason: t.get("reason").and_then(Value::as_str)
                .filter(|r| !matches!(*r, "Completed" | "Error"))
                .map(String::from),
        },
        ("Failed", None, _) => PodProgress::Finished {
            pod: name,
            exit_code: -1,
            reason: Some(pod.pointer("/status/reason").and_then(Value::as_str).unwrap_or("the pod failed").to_string()),
        },
        ("Running", None, _) => PodProgress::Running { pod: name },
        (phase, None, reason) => PodProgress::Waiting { pod: name, reason: reason.unwrap_or(phase).to_string() },
    }
}

#[must_use]
pub fn active_deadline_secs(payload: &JobPayload) -> Option<u64> {
    payload.command_limit_secs.map(|limit| 2 * limit + BACKSTOP_ALLOWANCE_SECS)
}

#[must_use]
pub fn job_manifest(name: &str, image: &str, active_deadline_secs: Option<u64>) -> Value {
    let mut spec = json!({
        "backoffLimit": 0,
        "ttlSecondsAfterFinished": TTL_AFTER_FINISHED_SECS,
        "template": {
            "metadata": { "labels": { "assembly-line/job": name } },
            "spec": {
                "restartPolicy": "Never",
                "containers": [{
                    "name": "job",
                    "image": image,
                    "command": ["assembly", "job-exec"],
                    "envFrom": [{ "secretRef": { "name": name } }],
                }],
            },
        },
    });
    if let Some(secs) = active_deadline_secs {
        spec["activeDeadlineSeconds"] = json!(secs);
    }
    json!({ "apiVersion": "batch/v1", "kind": "Job", "metadata": { "name": name }, "spec": spec })
}
```

(`spec` is built then conditionally extended — the one mutation, because
`json!` has no optional-key syntax.)

```rust
#[must_use]
pub fn secret_manifest(name: &str, job_uid: &str, vars: &BTreeMap<String, String>) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": name,
            "ownerReferences": [{ "apiVersion": "batch/v1", "kind": "Job", "name": name, "uid": job_uid }],
        },
        "type": "Opaque",
        "stringData": vars,
    })
}

/// `kubectl logs --timestamps` prefixes each line with an RFC 3339
/// timestamp and a space. The timestamp is where a resumed stream restarts.
#[must_use]
pub fn split_timestamp(line: &str) -> (Option<&str>, &str) {
    match line.split_once(' ') {
        Some((stamp, rest)) if chrono::DateTime::parse_from_rfc3339(stamp).is_ok() => (Some(stamp), rest),
        _ => (None, line),
    }
}
```

- [ ] **Step 4: Write the failing end-to-end runner tests**

Append to `tests/kubernetes_runner.rs` — a fake `kubectl` whose pod is
`Running` until its second `logs` call, and whose first `logs` call drops
after two lines:

```rust
use assembly_line::collect::collect;
use assembly_line::event::{EventKind, EventLog};
use assembly_line::frame::FrameWriter;
use assembly_line::runner::kubernetes::KubernetesRunner;
use assembly_line::runner::{JobSecrets, Runner};
use tokio_util::sync::CancellationToken;

mod support;
use support::{Harness, fake_cli};

/// A whole passing round's frames, as `kubectl logs --timestamps` prints
/// them.
fn timestamped_round() -> String {
    let frames = FrameWriter::new(Vec::new());
    frames.append_event(EventKind::JobStarted { round: 1 }).unwrap();
    frames.append_output("agent working").unwrap();
    frames.append_event(EventKind::JobFinished { exit_code: 0 }).unwrap();
    String::from_utf8(frames.copy_of_sink())
        .unwrap()
        .lines()
        .enumerate()
        .map(|(i, line)| format!("2026-09-21T10:00:0{i}Z {line}\n"))
        .collect()
}

fn fake_kubectl(dir: &std::path::Path) -> std::path::PathBuf {
    std::fs::write(dir.join("frames"), timestamped_round()).unwrap();
    fake_cli(
        dir,
        "kubectl",
        &format!(
            "cd {dir}\n\
             echo \"$*\" >> argv\n\
             case \"$*\" in\n\
               *'auth can-i'*) echo yes ;;\n\
               *apply*) input=$(cat); case \"$input\" in *'\"kind\":\"Job\"'*) echo '{{\"metadata\":{{\"uid\":\"uid-1\"}}}}' ;; esac ;;\n\
               *'get pods'*) if [ -e done ]; then echo '{{\"items\":[{{\"metadata\":{{\"name\":\"p\"}},\"status\":{{\"phase\":\"Succeeded\",\"containerStatuses\":[{{\"state\":{{\"terminated\":{{\"exitCode\":0}}}}}}]}}}}]}}'; \
                             else echo '{{\"items\":[{{\"metadata\":{{\"name\":\"p\"}},\"status\":{{\"phase\":\"Running\"}}}}]}}'; fi ;;\n\
               *logs*) case \"$*\" in *since-time*) cat frames; touch done ;; *) head -n 2 frames ;; esac ;;\n\
               *) ;;\n\
             esac\n",
            dir = dir.display()
        ),
    )
}

fn runner(program: std::path::PathBuf) -> KubernetesRunner {
    KubernetesRunner {
        program,
        poll_interval: std::time::Duration::from_millis(10),
        scheduling_deadline: std::time::Duration::from_secs(5),
        ..KubernetesRunner::new("img:1".into(), "factory".into(), None)
    }
}

/// A dropped log stream is resumed, and the frames it replays are not
/// appended twice.
#[tokio::test]
async fn a_dropped_log_stream_is_resumed_without_duplicating_events() {
    let h = Harness::new().await;
    let fakes = h.scratch_root().with_file_name("fakes");
    std::fs::create_dir_all(&fakes).unwrap();
    let k8s = runner(fake_kubectl(&fakes));
    let paths = h.job_paths();
    let mut log = EventLog::open_append(paths.events()).unwrap();

    let job = k8s.launch(&h.payload_for("x").await, &JobSecrets::default()).await.unwrap();
    let outcome = collect(job, &mut log, &paths.log(), CancellationToken::new()).await.unwrap();

    assert!(outcome.passed());
    let events = EventLog::read(paths.events()).unwrap();
    let started = events.iter().filter(|e| matches!(e.kind, EventKind::JobStarted { .. })).count();
    assert_eq!(started, 1, "a replayed frame was collected twice: {events:?}");

    let argv = std::fs::read_to_string(fakes.join("argv")).unwrap();
    assert!(argv.contains("--namespace factory"), "{argv}");
    assert!(argv.contains("--since-time=2026-09-21T10:00:01Z"), "resumed from the wrong place: {argv}");
    assert!(argv.contains("delete job"), "the Job — and its Secret — outlived the round: {argv}");
}

#[tokio::test]
async fn a_pod_that_never_starts_fails_at_the_scheduling_deadline_with_its_reason() {
    let h = Harness::new().await;
    let fakes = h.scratch_root().with_file_name("fakes");
    let program = fake_cli(
        &fakes,
        "kubectl",
        &format!(
            "echo \"$*\" >> {argv}\n\
             case \"$*\" in\n\
               *apply*) cat >/dev/null; echo '{{\"metadata\":{{\"uid\":\"u\"}}}}' ;;\n\
               *'get pods'*) echo '{{\"items\":[{{\"metadata\":{{\"name\":\"p\"}},\"status\":{{\"phase\":\"Pending\",\"containerStatuses\":[{{\"state\":{{\"waiting\":{{\"reason\":\"ImagePullBackOff\"}}}}}}]}}}}]}}' ;;\n\
             esac\n",
            argv = fakes.join("argv").display()
        ),
    );
    let k8s = KubernetesRunner { scheduling_deadline: std::time::Duration::from_millis(200), ..runner(program) };

    let err = k8s.launch(&h.payload_for("x").await, &JobSecrets::default()).await.unwrap_err().to_string();

    assert!(err.contains("ImagePullBackOff"), "{err}");
    let argv = std::fs::read_to_string(fakes.join("argv")).unwrap();
    assert!(argv.contains("delete job"), "the unstarted Job was left behind: {argv}");
}

#[tokio::test]
async fn missing_permissions_are_preflight_problems_naming_the_namespace() {
    let tmp = tempfile::tempdir().unwrap();
    let k8s = runner(fake_cli(tmp.path(), "kubectl", "echo no\n"));

    let problems: Vec<String> = k8s.reasons_it_cannot_run().await.iter().map(ToString::to_string).collect();

    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(problems.iter().all(|p| p.contains("'factory'")), "{problems:?}");
}
```

- [ ] **Step 5: Implement the runner half**

```rust
#[derive(Debug, Clone)]
pub struct KubernetesRunner {
    /// `kubectl`, or a stand-in a test wrote.
    pub program: PathBuf,
    pub image: String,
    /// Where every Job and Secret is created. Required: it is where the
    /// credentials land, so it is never left to a kubeconfig default.
    pub namespace: String,
    /// `kubectl`'s current context when `None`.
    pub context: Option<String>,
    pub scheduling_deadline: Duration,
    pub poll_interval: Duration,
}

impl KubernetesRunner {
    #[must_use]
    pub fn new(image: String, namespace: String, context: Option<String>) -> Self {
        KubernetesRunner {
            program: "kubectl".into(),
            image,
            namespace,
            context,
            scheduling_deadline: SCHEDULING_DEADLINE,
            poll_interval: POLL_INTERVAL,
        }
    }

    /// `kubectl` pinned to this runner's context and namespace.
    fn kubectl(&self) -> Command {
        let mut command = Command::new(&self.program);
        if let Some(context) = &self.context {
            command.args(["--context", context]);
        }
        command.args(["--namespace", &self.namespace]);
        command
    }
}

/// Run `command`, feeding it `stdin`, and return its stdout.
async fn output_of(mut command: Command, stdin: Option<String>) -> anyhow::Result<String> {
    let mut child = command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    if let (Some(body), Some(mut pipe)) = (stdin, child.stdin.take()) {
        pipe.write_all(body.as_bytes()).await?;
    }
    let out = child.wait_with_output().await?;
    anyhow::ensure!(
        out.status.success(),
        "kubectl failed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

impl Runner for KubernetesRunner {
    type Running = KubernetesJob;
    const RUNS_IN_A_CONTAINER: bool = true;

    async fn reasons_it_cannot_run(&self) -> Vec<RunnerProblem> {
        let mut problems = Vec::new();
        // A loop over two resources: each check is its own subprocess.
        for resource in ["jobs", "secrets"] {
            let mut command = self.kubectl();
            command.args(["auth", "can-i", "create", resource]);
            match output_of(command, None).await {
                Ok(answer) if answer.trim() == "yes" => {}
                Ok(_) => problems.push(RunnerProblem::CannotCreate {
                    resource: resource.into(),
                    namespace: self.namespace.clone(),
                }),
                Err(e) => problems.push(RunnerProblem::Unreachable { runner: "kubectl", detail: e.to_string() }),
            }
        }
        problems
    }

    async fn launch(&self, payload: &JobPayload, secrets: &JobSecrets) -> anyhow::Result<KubernetesJob> {
        let name = job_resource_name(payload);
        let mut apply_job = self.kubectl();
        apply_job.args(["apply", "-f", "-", "-o", "json"]);
        let created: Value = serde_json::from_str(
            &output_of(apply_job, Some(job_manifest(&name, &self.image, active_deadline_secs(payload)).to_string())).await?,
        )?;
        let uid = created.pointer("/metadata/uid").and_then(Value::as_str).unwrap_or_default().to_string();

        let vars: BTreeMap<String, String> = secrets
            .vars()
            .clone()
            .into_iter()
            .chain([(PAYLOAD_VAR.to_string(), serde_json::to_string(payload)?)])
            .collect();
        let mut apply_secret = self.kubectl();
        apply_secret.args(["apply", "-f", "-"]);
        output_of(apply_secret, Some(secret_manifest(&name, &uid, &vars).to_string())).await?;

        let job = KubernetesJob { runner: self.clone(), name, pod: String::new(), lines: None, resume_from: None };
        match job.wait_until_started().await {
            Ok(pod) => job.following(pod),
            Err(e) => {
                job.delete().await;
                Err(e)
            }
        }
    }
}
```

`KubernetesJob`:

```rust
#[derive(Debug)]
pub struct KubernetesJob {
    runner: KubernetesRunner,
    name: String,
    pod: String,
    lines: Option<ChildLines>,
    /// The timestamp of the last line read — where a resumed stream starts.
    resume_from: Option<String>,
}

impl KubernetesJob {
    async fn progress(&self) -> PodProgress {
        let mut command = self.runner.kubectl();
        command.args(["get", "pods", "-l", &format!("job-name={}", self.name), "-o", "json"]);
        output_of(command, None)
            .await
            .ok()
            .and_then(|out| serde_json::from_str(&out).ok())
            .map_or(PodProgress::Gone, |pods| pod_progress(&pods))
    }

    /// Poll until the pod runs or finishes; fail with the pod's own reason
    /// at the scheduling deadline.
    async fn wait_until_started(&self) -> anyhow::Result<String> {
        let started = std::time::Instant::now();
        // A loop: polling is sequential I/O with an exit on each branch.
        loop {
            match self.progress().await {
                PodProgress::Running { pod } | PodProgress::Finished { pod, .. } => return Ok(pod),
                waiting if started.elapsed() >= self.runner.scheduling_deadline => {
                    let reason = match waiting {
                        PodProgress::Waiting { reason, .. } => reason,
                        _ => "no pod was created".to_string(),
                    };
                    anyhow::bail!("the pod never started: {reason}");
                }
                _ => tokio::time::sleep(self.runner.poll_interval).await,
            }
        }
    }

    fn following(self, pod: String) -> anyhow::Result<KubernetesJob> {
        let lines = self.logs_of(&pod)?;
        Ok(KubernetesJob { pod, lines: Some(lines), ..self })
    }

    fn logs_of(&self, pod: &str) -> anyhow::Result<ChildLines> {
        let mut command = self.runner.kubectl();
        command.args(["logs", "-f", "--timestamps", &format!("pod/{pod}")]);
        if let Some(since) = &self.resume_from {
            command.arg(format!("--since-time={since}"));
        }
        ChildLines::spawn(command)
    }

    /// Deleting the Job cascades to its pod and — through its owner
    /// reference — its Secret. The Secret is deleted explicitly too, in case
    /// the owner reference was never set.
    async fn delete(&self) {
        let mut job = self.runner.kubectl();
        job.args(["delete", "job", &self.name, "--ignore-not-found", "--wait=false"]);
        let _ = output_of(job, None).await;
        let mut secret = self.runner.kubectl();
        secret.args(["delete", "secret", &self.name, "--ignore-not-found", "--wait=false"]);
        let _ = output_of(secret, None).await;
    }
}

impl RunningJob for KubernetesJob {
    /// A `kubectl logs -f` that ends while the pod is still running dropped;
    /// start another from the last timestamp. The collector drops what it
    /// replays by `seq`.
    async fn next_line(&mut self) -> Option<String> {
        // A loop: reconnecting is sequential I/O until a line or the end.
        loop {
            let lines = self.lines.as_mut()?;
            match lines.next_line().await {
                Some(line) => {
                    let (stamp, text) = split_timestamp(&line);
                    if let Some(stamp) = stamp {
                        self.resume_from = Some(stamp.to_string());
                    }
                    return Some(text.to_string());
                }
                None => match self.progress().await {
                    PodProgress::Running { .. } => {
                        tokio::time::sleep(self.runner.poll_interval).await;
                        self.lines = self.logs_of(&self.pod).ok();
                    }
                    _ => {
                        self.lines = None;
                        return None;
                    }
                },
            }
        }
    }

    async fn cancel(&mut self) {
        self.delete().await;
    }

    async fn termination(self) -> Termination {
        let progress = self.progress().await;
        self.delete().await;
        match progress {
            PodProgress::Finished { reason: Some(reason), .. } => Termination::Killed { reason },
            PodProgress::Finished { exit_code, .. } => Termination::Exited(exit_code),
            PodProgress::Gone => Termination::Killed { reason: "the pod was deleted before it finished".into() },
            PodProgress::Waiting { reason, .. } => Termination::Killed { reason },
            PodProgress::Running { .. } => Termination::Killed { reason: "the log stream ended while the pod was running".into() },
        }
    }
}
```

Add `pub mod kubernetes;` to `src/runner/mod.rs`.

- [ ] **Step 6: CLI surface**

`RunnerKind` gains:

```rust
    /// A k8s Job, through the `kubectl` CLI
    #[value(name = "k8s")]
    K8s,
```

`RunnerArgs` gains:

```rust
    /// Where k8s Jobs and their Secrets are created. Required for k8s.
    #[arg(long, required_if_eq("runner", "k8s"))]
    pub namespace: Option<String>,
    /// The kubectl context. Defaults to kubectl's current one.
    #[arg(long)]
    pub context: Option<String>,
```

`run_work_on_chosen_runner` refuses `--namespace`/`--context` for non-k8s
runners with "--namespace and --context apply to the k8s runner", and adds:

```rust
        RunnerKind::K8s => run_work(
            &KubernetesRunner::new(image, args.namespace.clone().unwrap_or_default(), args.context.clone()),
            &args.pass_env,
            work,
        )
        .await,
```

(`unwrap_or_default` is unreachable: clap has already required it.)

Append to `tests/cli.rs`:

```rust
#[tokio::test]
async fn the_k8s_runner_requires_a_namespace() {
    let tmp = repo_running("fake-agent.sh").await;
    assembly(&tmp)
        .args(["run", "--prompt", "x", "--runner", "k8s"])
        .assert()
        .code(2)
        .stderr(contains("--namespace"));
    discard_origin(&tmp);
}

#[tokio::test]
async fn a_namespace_is_refused_for_runners_that_have_none() {
    let tmp = repo_running("fake-agent.sh").await;
    assembly(&tmp)
        .args(["run", "--prompt", "x", "--runner", "docker", "--namespace", "factory"])
        .assert()
        .code(2)
        .stderr(contains("k8s runner"));
    discard_origin(&tmp);
}
```

- [ ] **Step 7: Run everything**

Run: `just check`
Expected: green.

- [ ] **Step 8: Commit**

```bash
jj describe -m "feat(runner): a k8s runner driving kubectl

Each round is a Job (backoffLimit 0, activeDeadlineSeconds as a backstop at
2x max_duration + 30m) whose environment comes from a per-round Secret the
Job owns. A pod unstarted after 10 minutes fails the round with the pod's
own reason. kubectl logs -f is resumed from its last timestamp when it
drops, and the collector drops the replayed frames by seq. The Job and
Secret are deleted when the round ends or is cancelled. --namespace is
required; --context defaults to kubectl's.

Tests: <count>."
jj new
```

---

### Task 10: The job image

**Files:**
- Create: `Dockerfile`, `.dockerignore`, `scripts/smoke-docker.sh`
- Modify: `justfile`, `renovate.json`

**Interfaces:** none in Rust. Produces an image whose `assembly job-exec`
matches the binary's version, with `git`, `mise`, `claude`, `codex`,
`opencode` on `PATH`, `mise` data at `/mise`, running as a non-root user.

- [ ] **Step 1: Write `.dockerignore`**

```
target/
.git/
.jj/
.devenv/
.direnv/
.assembly/
docs/
```

- [ ] **Step 2: Write the `Dockerfile`**

```dockerfile
# syntax=docker/dockerfile:1

# The job image: everything a round needs except the repository's own
# toolchain, which `job-exec` provisions with mise at the start of the round.

FROM rust:1.97.1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked --bin assembly

FROM debian:bookworm-slim

# Pinned for renovate; bump together with a release.
ARG MISE_VERSION=v2026.9.0
ARG CLAUDE_CODE_VERSION=latest
ARG CODEX_VERSION=latest
ARG OPENCODE_VERSION=latest

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl git xz-utils unzip \
 && rm -rf /var/lib/apt/lists/*

# Claude Code refuses to skip permission prompts as root, and an agent has
# no business being root anyway.
RUN useradd --create-home --uid 1000 agent \
 && mkdir -p /mise \
 && chown agent:agent /mise

# /mise is where the docker runner mounts its cache volume. Owning it here
# means a fresh volume inherits the ownership.
ENV MISE_DATA_DIR=/mise \
    MISE_CACHE_DIR=/mise/cache \
    MISE_YES=1 \
    PATH=/mise/shims:/home/agent/.local/bin:/usr/local/bin:/usr/bin:/bin

RUN curl -fsSL https://mise.run | MISE_INSTALL_PATH=/usr/local/bin/mise MISE_VERSION=${MISE_VERSION} sh

COPY --from=build /src/target/release/assembly /usr/local/bin/assembly

USER agent
WORKDIR /home/agent

# Native builds, not npm: an npm-installed agent runs on whatever `node` the
# repository's mise config pins, which may be one the agent does not support.
RUN curl -fsSL https://claude.ai/install.sh | bash -s -- ${CLAUDE_CODE_VERSION} \
 && curl -fsSL https://opencode.ai/install | bash \
 && mkdir -p /home/agent/.local/bin \
 && curl -fsSL "https://github.com/openai/codex/releases/${CODEX_VERSION}/download/codex-$(uname -m)-unknown-linux-musl.tar.gz" \
    | tar -xz -C /home/agent/.local/bin \
 && mv /home/agent/.local/bin/codex-* /home/agent/.local/bin/codex

# Fail the build, not the first job, if anything above is missing.
RUN assembly --version && git --version && mise --version \
 && claude --version && codex --version && opencode --version

ENTRYPOINT []
CMD ["assembly", "job-exec"]
```

The three vendor installers are the part most likely to drift. Before
committing, check each against its vendor's current install docs; if
`opencode`'s installer puts the binary somewhere other than
`~/.local/bin` or `~/.opencode/bin`, add that directory to `PATH`. The
final `RUN … --version` line is what proves it: the image does not build
until all six commands resolve.

- [ ] **Step 3: Add the just recipes**

```make
# Build the job image locally, for the host's platform.
image tag="assembly-line:dev":
    docker buildx build --load -t {{tag}} .

# Boot the built image against a scratch repository and prove job-exec runs
# a round end to end. Needs a docker daemon; not part of `just check`.
smoke-docker tag="assembly-line:dev": (image tag)
    scripts/smoke-docker.sh {{tag}}
```

- [ ] **Step 4: Write `scripts/smoke-docker.sh`**

```bash
#!/usr/bin/env bash
# Runs one round in the real image, against a bare repository mounted into
# the container, with a shell one-liner standing in for the agent. Proves
# the image boots job-exec, provisions nothing it should not, commits,
# pushes, and prints frames. The runner itself is covered by `cargo test`'s
# fakes; this covers what the fakes cannot: the image.
set -euo pipefail

image="${1:?usage: smoke-docker.sh <image>}"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

git init --quiet --initial-branch=main "$work/repo"
git -C "$work/repo" -c user.name=smoke -c user.email=smoke@localhost commit --quiet --allow-empty -m base
git init --quiet --bare --initial-branch=main "$work/origin.git"
git -C "$work/repo" push --quiet "$work/origin.git" main
# The container runs as uid 1000 and must be able to push.
chmod -R a+rwX "$work/origin.git"
sha=$(git -C "$work/repo" rev-parse HEAD)

payload=$(cat <<JSON
{"job_id":1,"round":1,"remote_url":"/origin.git","remote_name":"origin",
 "start":{"name":"main","sha":"$sha"},"branch":"al/job-1",
 "command":{"program":"sh","args":["-c","echo smoke > smoke.txt"]},
 "commit_message":"job 1: smoke","verify":"test -f smoke.txt",
 "command_limit_secs":300,"copy":[],"seed_from":"/nonexistent",
 "provision_toolchain":true}
JSON
)

out=$(docker run --rm -e ASSEMBLY_JOB="$payload" -v "$work/origin.git:/origin.git" "$image")
echo "$out"

echo "$out" | grep -q '"t":"job_finished"' || { echo "smoke: no job_finished frame" >&2; exit 1; }
git -C "$work/origin.git" show al/job-1:smoke.txt | grep -q smoke || { echo "smoke: branch not pushed" >&2; exit 1; }
echo "smoke: ok"
```

`chmod +x scripts/smoke-docker.sh` (file mode, not content — allowed).

- [ ] **Step 5: Let renovate see the pins**

Add to `renovate.json` a regex manager for the `ARG *_VERSION=` lines in
`Dockerfile` (`"customManagers": [{ "customType": "regex", "fileMatch": ["^Dockerfile$"], "matchStrings": ["ARG MISE_VERSION=(?<currentValue>v[\\d.]+)"], "depNameTemplate": "jdx/mise", "datasourceTemplate": "github-releases" }]`).
The agents stay on `latest` until a release pins them.

- [ ] **Step 6: Verify**

Run: `just check` (unchanged — nothing in Rust moved), then, on a machine
with Docker: `just smoke-docker`.
Expected: `smoke: ok`. If there is no Docker daemon available, say so in
the commit body rather than claiming the smoke test passed.

- [ ] **Step 7: Commit**

```bash
jj describe -m "build(image): the one job image, and a smoke test for it

A multi-stage Dockerfile: assembly built from source, git, mise at /mise
(the docker runner's cache volume), and native Claude Code, Codex and
opencode, running as a non-root user. The build fails if any of them is
missing. just image builds it; just smoke-docker runs one real round in it
against a mounted bare repository.

Tests: <count> (unchanged). Smoke: <ok | not run — no docker daemon>."
jj new
```

---

### Task 11: Publish the image on a version tag

**Files:**
- Create: `.github/workflows/publish-image.yml`

**Interfaces:** pushing tag `vX.Y.Z` publishes
`ghcr.io/hmbill694/assembly-line:X.Y.Z` for `linux/amd64` and `linux/arm64`.
That is exactly what `runner::published_image()` names for a binary at
`X.Y.Z`.

- [ ] **Step 1: Write the workflow**

```yaml
# Publishes the job image the docker and k8s runners launch. The tag must
# match Cargo.toml's version exactly: a binary at X.Y.Z launches the image
# tagged X.Y.Z, and nothing else.
name: publish-image

on:
  push:
    tags: ["v*"]

permissions:
  contents: read
  packages: write

jobs:
  publish:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: The tag matches the crate version
        run: |
          crate=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
          test "${GITHUB_REF_NAME#v}" = "$crate" \
            || { echo "tag $GITHUB_REF_NAME does not match Cargo.toml version $crate" >&2; exit 1; }
          echo "VERSION=$crate" >> "$GITHUB_ENV"

      - uses: docker/setup-qemu-action@v3
      - uses: docker/setup-buildx-action@v3

      - uses: docker/login-action@v3
        with:
          registry: ghcr.io
          username: ${{ github.actor }}
          password: ${{ secrets.GITHUB_TOKEN }}

      - uses: docker/build-push-action@v6
        with:
          context: .
          platforms: linux/amd64,linux/arm64
          push: true
          tags: ghcr.io/hmbill694/assembly-line:${{ env.VERSION }}
          cache-from: type=gha
          cache-to: type=gha,mode=max
```

- [ ] **Step 2: Verify**

There is no local runner for Actions in this repo. Check the YAML parses:
`python3 -c 'import yaml,sys; yaml.safe_load(open(".github/workflows/publish-image.yml"))'`
(reading only). The real verification is the first tag push, which is the
user's call — do not push a tag.

- [ ] **Step 3: Commit**

```bash
jj describe -m "ci: publish the job image to GHCR on a version tag

Multi-arch (amd64, arm64), tagged with the crate version the tag names —
the image a binary at that version launches. Refuses a tag that does not
match Cargo.toml. The rest of CI is a separate effort.

Tests: <count> (unchanged)."
jj new
```

- [ ] **Step 4: Verify the whole stack**

Run: `just verify-stack`
Expected: every revision from Task 2 up builds, tests, lints and formats
cleanly on its own; test counts fall in Tasks 3–4 and climb from Task 5.

---

## Not F2: mise-nix — system packages in the one image

**This is F8, after F7.** It is recorded here because F2 is what creates the
gap and what the fix builds on, but no part of it belongs in this stack or
this milestone. It closes the gap F2 leaves open on purpose: a repository
whose `verify` needs a system package (`protoc`, `libssl-dev`, a Postgres
for tests) cannot run in a container, and there is deliberately no `setup`
field to paper over it. Until F8, such a repository uses the local runner.

**Goal:** A repository declares system packages in its own `mise.toml` —
e.g. `[tools] "nix:protobuf" = "latest"` — and container rounds get them
from the same `mise install` F2 already runs. No new config field, no change
to the job contract, the payload, or any runner.

**Why it is purely additive:** `job-exec` already runs `mise trust` and
`mise install` in the clone. mise-nix is a mise backend; once the image has
Nix and the plugin, `mise install` resolves `nix:` tools with no code change.

**Sketch, to be turned into an F8 plan when F7 is done:**

1. **Nix in the image.** Single-user Nix owned by the `agent` user
   (`/nix` created and chowned before `USER agent`), flakes enabled in
   `/etc/nix/nix.conf`. The final `--version` check gains `nix --version`.
2. **The mise-nix plugin in the image.** `mise plugins install nix <mise-nix repo URL>`
   as the `agent` user, pinned by commit, with a renovate rule.
3. **Cache.** Docker: a second named volume for `/nix`, mounted by the
   docker runner beside `assembly-mise-cache` (a change to
   `docker_run_args` and its test). k8s stays cold, as in F2.
4. **Smoke.** `scripts/smoke-docker.sh` gains a second round whose payload's
   clone carries a `mise.toml` with `"nix:protobuf" = "latest"` and whose
   `verify` is `protoc --version`.
5. **Spec.** Accepted risk 9 is removed; "The job image" section says
   system packages come through `mise.toml`.

Nothing above is scheduled until F7 ships. If a repository needs a system
package before then, the answer is `--runner local`, not an early start on
this.

**Open question to settle before starting:** image size. Nix plus a warm
store is large; decide whether the store lives only in the cache volume
(small image, slow first job) or partly in the image.
