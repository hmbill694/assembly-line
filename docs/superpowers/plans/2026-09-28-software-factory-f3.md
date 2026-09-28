# Software Factory F3 Implementation Plan — the daemon

**Status:** planned 2026-09-28.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `assembly run` is one whole job in one process — the command every
runner launches and the command a human types to reproduce one — and a
long-lived `assembly daemon` owns a state root, one runner and a socket:
it preflights and claims jobs at submit, queues them under a concurrency cap,
launches `run` on its runner, collects what it reports, cancels on request,
and reattaches to its rounds after a restart.

**Architecture:** The job boundary moves out from a round (`job-exec`, fed an
`ASSEMBLY_JOB` payload) to the whole job (`assembly run`, fed plain flags):
`run` clones, reads config at the pinned base SHA, claims an id by creating
`al/job-N` on the remote, runs the round, pushes and opens the pull request.
The daemon knows *where* a job runs and *whether* it would be refused, never
how it is done. Its only memory is the event logs under its root, folded
with `JobReport::from_events`; the CLI reaches it by HTTP over a Unix socket.

**Tech Stack:** Rust 2024 (stable 1.98.1, pinned), tokio, clap (+`env`),
serde/serde_json, anyhow, axum (server over `UnixListener`), hyper +
hyper-util + http-body-util (client over `UnixStream`), nix (`fs` for
`flock`), git/docker/kubectl/gh via subprocess. jj, colocated with git.

**Spec:** `docs/superpowers/specs/2026-09-11-software-factory-v2.md`, as
amended on 2026-09-28 by change `nmoxrmnu` (`docs(spec): amend the spec with
the F3 decisions`), which is already in the stack below this plan's first
task. Read *The daemon*, *Launching and watching*, *Claiming a job's id*,
*The runner* and *Invariants* before Task 1.

## Decisions this plan implements

Agreed in the F3 design interview on 2026-09-27/28. Reopening one is a design
change, not a clarification.

| # | Decision |
|---|---|
| 1 | F3's daemon is a job host people use today, before F4 gives it sources. |
| 2 | The CLI and daemon share a machine in F3. Reaching a daemon from elsewhere arrives with F6's web UI, which brings a TCP listener and auth. |
| 3 | The daemon is the only thing that runs a round for someone else; `submit` refuses when no daemon listens. |
| 4 | A runner launches `assembly run`, the command a human types, so a pod's command line is its reproduction. `job-exec` and `ASSEMBLY_JOB` are deleted. |
| 5 | The daemon launches, watches, cancels and reattaches. How a job is done — config, claim, agent, `verify`, push, PR — is `run`'s alone. |
| 6 | A revise is `run --job N` with a new prompt. The agent gets only that prompt. `assembly revise` and `revised_prompt` are deleted. |
| 7 | Every commit a round makes carries its whole prompt in the commit body. |
| 8 | Only the daemon keeps state, under its root. A hand-typed `run` keeps none. `.assembly/jobs/` and `meta.json` are deleted. |
| 9 | The CLI reaches the daemon by HTTP over a Unix socket in the root. |
| 10 | One runner per daemon, configured by the flags `run` used to take, plus `--max-jobs`. No daemon config file in F3. |
| 11 | `submit` returns once the job is queued. `logs -f` watches; `cancel` stops. |
| 12 | Reattach works for every runner: the local runner writes a round's frames to a file. |
| 13 | `copy` is deleted. A config that declares it is refused with what to do instead. |
| 14 | The toolchain stays cold. A cache is a stretch goal after F7. |
| 15 | The daemon claims a job's id at submit by creating `al/job-N` on the remote; `run` without `--job` claims the same way. |
| 16 | The daemon preflights at submit: base pinned, config validated at that SHA, the job checked against its runner — before anything is claimed. |
| 17 | Streaming frames over the API waits for F6; `logs -f` is `tail -f`. |
| 18 | The daemon's forge token is `GH_TOKEN`, sent to container rounds alongside `ASSEMBLY_GIT_TOKEN`, withheld from the agent, `gh` added to the image. |

Decided while planning, within those:

| # | Decision |
|---|---|
| P1 | Rounds are numbered by whoever collects them. `run` emits no `RoundStarted`; the collector writes it before launching. |
| P2 | Every argument the daemon puts on `run`'s command line is `--flag=value`, so a prompt starting with `-` is a value, not a flag. |
| P3 | A job's identity is its first `RoundRequested` event, not a `meta.json`. Every submit — new job or revise — appends one. |
| P4 | A repository's place under the root is its `RepoKey`: host then path, from the remote URL's HTTPS form, so SSH and HTTPS spellings of one repository share a key. A local path's host is `local`. |
| P5 | The daemon serializes git work per repository cache: two fetches into one bare repository race on `FETCH_HEAD`. |
| P6 | Reattach resumes from a per-round `position` file the collector rewrites after each frame. At most one line is repeated after a crash between the two writes. |
| P7 | Until reattach lands (Task 12), stopping the daemon cancels its rounds and waits for their verdicts, so no log is left without one. |

## Global Constraints

Copied from `CLAUDE.md` and the spec. Every task's requirements implicitly
include this section.

- **Edition 2024, pinned to stable 1.98.1.** Never downgrade the edition to
  work around a compile error — fix the code.
- **All file modifications go through the Edit / Write tools.** Never `sed -i`,
  `perl -pi`, heredocs, `cat >`, `tee`, or inline scripts that write files.
  Reading and searching via shell is fine. Deleting a whole file a task says
  to delete is `rm`.
- **Every change in the jj stack must build, test, lint and format cleanly on
  its own.** `just check` (= `fmt-check` + `lint` + `test`) is the gate;
  `just verify-stack` checks every revision.
- **One concern per change**, each carrying the tests for what it adds or
  changes. A test file belongs to the change that introduces the module it
  imports.
- **Functional by default.** Iterators over `for` loops; `match` on the shape
  of data over if-else chains; build values rather than mutating them. Loops
  are correct for sequential I/O with early return — the claim retry, the
  dispatcher and the file tail below are such loops, and each says so.
- **Naming.** Predicates read as claims. Filters name what they select. Error
  producers name the fault. Mutators name the transition including its scope.
  Fields carry their unit or role. Constructors say where the value came from.
- **No trait until a second implementor exists.** `Runner` and `RunningRound`
  already have three; nothing in F3 adds a trait.
- **`async fn` in a public trait trips `async_fn_in_trait`** under
  `-D warnings`. Write trait methods as `fn ... -> impl Future<Output = T> + Send`.
- **Validation returns a `Vec` of typed errors**, and every error's `Display`
  says what to do about it.
- **The event log is append-only.** `JobReport::from_events` stays the only
  fold over a job's events, and pure. The `position` file (P6) is not the
  event log and is rewritten.
- **The factory never writes to a user's repository** — not its working tree,
  and from Task 10 on not its `.git` either.
- **`main.rs` is logicless** (`docs/superpowers/plans/2026-09-26-job-lifecycle-alignment.md`):
  it parses arguments, reads env, handles signals, runs the runtime, picks
  stdout or stderr, maps exit codes and builds the concrete runner. Every
  domain decision and message lives in the library.
- **No test may touch the network, a real Docker daemon, a real cluster, the
  developer's real `gh`, or `~/.local/state`.** Every test that runs the
  binary sets `ASSEMBLY_ROOT` to a tempdir. Agents are the shell-script fakes
  in `tests/fixtures/`; `docker`, `kubectl`, `mise` and `gh` are fakes written
  by the test that needs them.
- **Prove concurrency with observable evidence** — Task 10's cap is proven by
  a probe agent that records how many copies of itself were live at once.
- **Test counts climb monotonically**, except in Tasks 1, 5 and 7, which
  delete `copy`, `revise` and `job-exec` and their tests. Record the count in
  every commit body.

## Review Focus

The five inputs the spec implies but no happy-path test exercises, most
likely to bite first. Each has its test in the owning task.

1. **A prompt that begins with `-`, or spans lines with quotes in them.** A
   person expects the agent to receive it byte for byte. The daemon builds
   `run`'s argv, and `--prompt -x` would be parsed as a flag. Pinned in Task 7
   (`a_prompt_that_looks_like_a_flag_reaches_the_agent_intact`).
2. **One repository named by its SSH URL in one place and its HTTPS URL in
   another.** A person expects one set of job ids, not two. Pinned in Task 4
   (`ssh_and_https_spellings_of_one_repository_share_a_key`).
3. **A daemon killed with SIGKILL, leaving its socket and lock behind.** A
   person expects the next `assembly daemon` to start, not to report the
   root as taken by a dead process. Pinned in Task 8
   (`a_daemon_killed_outright_leaves_a_root_the_next_one_can_take`).
4. **Two submits to one repository at the same instant.** A person expects
   two jobs with different ids, both running. Pinned in Task 10
   (`two_submits_at_once_to_one_repository_get_two_jobs`).
5. **A `--root` long enough that its socket path passes the platform's limit
   (104 bytes on macOS).** A person expects the daemon to refuse with a
   message naming `--root`, not a bare `EINVAL`. Pinned in Task 8
   (`a_root_too_deep_for_a_socket_is_refused_by_name`).

---

## Version control

jj, colocated with git. The spec amendment is change `nmoxrmnu`, and this
plan is the change on top of it (`docs(plan): the F3 implementation plan`).
Start the first task on top of the plan:

```bash
jj new 'description(substring:"docs(plan): the F3 implementation plan")'
```

After each task:

```bash
jj describe -m "<conventional commit subject>

<body: what this changes and why, and the test count after>"
jj new
```

Never `jj edit` down the stack to verify. `just verify-stack` exports each
revision with `git archive` and builds it in a temp directory. Never set
`GIT_DIR` while verifying — a leaked `GIT_DIR` once let tests write into this
repository's own `.git`.

## The stack

| Task | Change | Behavior change |
|---|---|---|
| — | `docs(spec)`: amend the spec with the F3 decisions (`nmoxrmnu`, landed) | — |
| 1 | `refactor!`: delete `copy` | **yes** — a config declaring it is refused |
| 2 | `feat(claim)`: a job's id is claimed by creating its branch on the remote | **yes** — a job's branch exists from the start |
| 3 | `feat(round)`: a round's commit carries its whole prompt | commit bodies |
| 4 | `refactor!`: job state lives under a state root, not in the repository | **yes** — `.assembly/jobs/` gone; `--root` |
| 5 | `refactor(cli)!`: `submit` hands a job to a runner; a revise is `submit --job` | **yes** — `run`/`revise` renamed |
| 6 | `feat(run)`: `assembly run` does one whole job in this process | new command |
| 7 | `refactor(runner)!`: runners launch `assembly run`; `job-exec` and `ASSEMBLY_JOB` go | **yes** — image, `GH_TOKEN` |
| 8 | `feat(daemon)`: `assembly daemon` holds a root, a runner and a socket | new command |
| 9 | `test`: the harness runs a job the way `assembly run` does | — |
| 10 | `feat(daemon)!`: `submit` queues a job with the daemon, preflighted and claimed | **yes** — `submit` returns at once |
| 11 | `feat(daemon)`: `cancel` stops a queued or running job | new command |
| 12 | `feat(daemon)`: a restarted daemon reattaches to the rounds it left running | **yes** — stopping stops no job |
| 13 | `docs`: `CLAUDE.md`, the justfile demo and the plan's status follow the new boundary | — |

Why this order: every change must build and pass on its own, so the host role
moves in steps rather than all at once. Task 5 renames the orchestrating
command to `submit`, freeing the name `run`; Task 6 builds the whole-job
`run` beside `job-exec`; Task 7 switches the runners to it and deletes
`job-exec`; Task 9 moves the test harness off the host code, and Tasks 8
and 10 move `submit`'s orchestration into the daemon. Between Tasks 5 and
10, `submit` runs its round in the foreground as `run` did before — that is
the transitional host, and Task 10 retires it.

## File Structure

What each file is responsible for once F3 is done.

| File | Responsibility after F3 |
|---|---|
| `src/cli.rs` | `run`, `submit`, `daemon`, `status`, `logs`, `cancel`; global `--root`; `RunnerArgs` on `daemon` |
| `src/run.rs` | **new** (Task 6) — `assembly run`: resolve, clone, config, claim, round, deliver, conclude |
| `src/claim.rs` | **new** (Task 2) — `claim_job`: the next id, by creating its branch on the remote |
| `src/locate.rs` | **new** (Task 4) — from what the user typed (`--repo`, a job id) to a `RepoKey` and a job directory under the root |
| `src/daemon/mod.rs` | **new** (Task 8) — `Daemon`, `serve`, shutdown |
| `src/daemon/root.rs` | **new** (Task 8) — the root lock, the socket path and its length limit |
| `src/daemon/api.rs` | **new** (Task 8, grows in 10–11) — wire types and the axum router |
| `src/daemon/client.rs` | **new** (Task 8) — the CLI's HTTP-over-UDS client |
| `src/daemon/submit.rs` | **new** (Task 10) — preflight at submit, in the bare cache, then claim and record |
| `src/daemon/dispatch.rs` | **new** (Task 10) — the queue, the cap, launch and collect |
| `src/daemon/fleet.rs` | **new** (Task 12) — the startup fold: what to requeue, reattach or close |
| `src/submission.rs` | **new** (Task 10) — `submit`'s side: the checkout's remote, the ref, the "your ref differs" note |
| `src/runner/mod.rs` | `Runner` (launch + reattach), `RunningRound` (+ `handle`), `LaunchSpec`, `RoundHandle`, `JobSecrets` |
| `src/runner/local.rs` | `assembly run` in a session of its own, frames to a file; `FileTail` |
| `src/runner/docker.rs` | `docker create` + `start`, `logs -f`, `wait`, `stop`, `rm` |
| `src/runner/kubernetes.rs` | Job + Secret running `assembly run`; reattach by Job name |
| `src/collect.rs` | Stream → `events.jsonl` + log + `position`; resumes after a seq |
| `src/event.rs` | + `RoundRequested`, `PullRequestOpened`, `RoundLaunched` |
| `src/report.rs` | `JobReport` gains the job's identity, pull request and launch handle; `JobState::Queued` |
| `src/paths.rs` | State root, `RepoKey`, job directories; no `meta.json`, no `.assembly/jobs` |
| `src/payload.rs` | `RoundPayload` — the in-process plan for one round, no longer serialised; tokens' names |
| `src/round.rs` | `run_round_in`: a round in a clone it is handed |
| `src/workspace.rs` | `clone_scratch`, `pin_in_clone`, `start_round`, commit, publish, discard; no seeding |
| `src/delivery.rs` | `gh` from inside the clone; an already-open pull request is not a failure |
| `src/lifecycle.rs` | **deleted** (Task 10) — its host half moves to `daemon::submit`, its reading half to `locate` |
| `Dockerfile`, `scripts/smoke-docker.sh` | `gh` added; the smoke test runs `assembly run` |

---

### Task 1: Delete `copy`

`copy` seeded gitignored files from a human's checkout into a round's clone
and kept them out of every commit. Nothing on the factory's path has a
checkout to copy from (Decision 13), so the field, the seeding, the
never-commit machinery and the container refusal all go. A config that still
declares `copy` is refused with what to do instead — `deny_unknown_fields`
alone would only say "unknown field".

**Files:**
- Modify: `src/config.rs` (field, `ConfigError::CopyRetired`)
- Modify: `src/git.rs` (`commit_all_except` → `commit_all`; delete `ensure_never_committed_since`)
- Modify: `src/workspace.rs` (no seeding)
- Modify: `src/payload.rs` (`copy`, `seed_from` go)
- Modify: `src/runner/mod.rs` (`CopyNeedsLocalRunner` goes; `copy` parameters go)
- Modify: `src/lifecycle.rs`, `src/round.rs` (callers)
- Test: `tests/config.rs`, `tests/repo_config.rs`, `tests/git.rs`, `tests/workspace.rs`, `tests/round.rs`, `tests/runner.rs`, `tests/payload.rs`, `tests/kubernetes_runner.rs`, `tests/cli.rs`, `tests/support/mod.rs`

**Interfaces:**
- Produces: `config::ConfigError::CopyRetired`; `RepoConfig::retired_copy: Option<toml::Value>`;
  `git::commit_all(clone: impl AsRef<Path>, message: &str) -> anyhow::Result<Option<String>>`;
  `workspace::create(remote_url: &str, start: &PinnedRef, branch: &str, scratch_root: impl AsRef<Path>, credential_helper: Option<&str>) -> anyhow::Result<RoundWorkspace>`;
  `runner::reasons_a_container_cannot_run(remote_url: &str) -> Vec<RunnerProblem>`;
  `runner::secrets_or_reasons_it_cannot_run(runner, remote_url, pass_env, host_environment)`.

- [ ] **Step 1: Write the failing test**

Add to `tests/config.rs`, and delete the `copy = [...]` line from `FULL` and
the `assert_eq!(config.copy.len(), 2);` line from
`parses_everything_a_repository_can_declare`:

```rust
#[test]
fn a_config_that_still_declares_copy_is_told_what_to_do_instead() {
    let config = RepoConfig::parse(
        "provider = \"p\"\ncopy = [\".env\"]\n[providers.p]\ncmd = \"p\"\n",
    )
    .expect("a retired field still parses, so it can be explained");

    let problems = config.reasons_it_cannot_run("p");

    assert_eq!(problems, [ConfigError::CopyRetired]);
    let said = problems[0].to_string();
    assert!(said.contains("commit") && said.contains("--pass-env"), "{said}");
}
```

Add `ConfigError::CopyRetired` to the list in `every_config_error_says_what_to_do_about_it`.

- [ ] **Step 2: Run it to make sure it fails**

Run: `cargo test --test config a_config_that_still_declares_copy`
Expected: FAIL to compile — `no variant named CopyRetired`.

- [ ] **Step 3: Retire the field in `src/config.rs`**

Replace the `copy` field of `RepoConfig` with:

```rust
    /// `copy`, deleted in F3. Parsed only so that a config still declaring
    /// it hears what to do instead, rather than just "unknown field".
    #[serde(default, rename = "copy")]
    pub retired_copy: Option<toml::Value>,
```

Add the variant and its message:

```rust
pub enum ConfigError {
    UnknownProvider(String),
    NoProviderDeclared,
    UnparseableMaxDuration(String),
    CopyRetired,
}
```

```rust
            Self::CopyRetired => write!(
                f,
                "`copy` is no longer supported — commit the file to the repository, or pass \
                 a secret to the round with the runner's --pass-env"
            ),
```

and chain it into `reasons_it_cannot_run`:

```rust
        undeclared
            .into_iter()
            .chain(self.unparseable_max_duration())
            .chain(self.retired_copy.is_some().then_some(ConfigError::CopyRetired))
            .collect()
```

Delete the `REPO_CONFIG_PATH` doc paragraph about ignoring `.assembly/jobs/`
only in Task 4, not here.

- [ ] **Step 4: Run it to make sure it passes**

Run: `cargo test --test config`
Expected: PASS.

- [ ] **Step 5: Delete the seeding and never-commit machinery**

`src/git.rs`: replace `commit_all_except` and delete
`ensure_never_committed_since` entirely:

```rust
/// Stage everything in `clone` and commit it, returning the new commit, or
/// `None` when there was nothing to commit.
pub async fn commit_all(clone: impl AsRef<Path>, message: &str) -> anyhow::Result<Option<String>> {
    let clone = clone.as_ref();
    run_expecting_success(clone, &["add", "-A"], "add -A").await?;

    let nothing_staged = run_allowing_failure(clone, &["diff", "--cached", "--quiet"])
        .await?
        .succeeded();
    match nothing_staged {
        true => Ok(None),
        false => {
            run_expecting_success(clone, &["commit", "--no-verify", "-m", message], "commit")
                .await?;
            head_sha(clone).await.map(Some)
        }
    }
}
```

`src/workspace.rs`: drop the `seed_from` and `copy_paths` parameters from
`create`, the `missing_seed_path` and `seed_files` functions, the `seeded`
field of `RoundWorkspace`, and the `std::os::unix::fs::PermissionsExt`
import only if nothing else uses it (`restore_owner_permissions` does — keep
it). The module doc becomes `//! One round's sandbox: a scratch clone of the
remote.` `create` becomes:

```rust
/// Clone `remote_url` into a fresh directory under `scratch_root`, with
/// `branch` checked out at `start`. A `credential_helper` authenticates both
/// the clone and the eventual push.
///
/// # Errors
///
/// A clone that fails midway leaves nothing: the directory is removed as
/// the error propagates.
pub async fn create(
    remote_url: &str,
    start: &PinnedRef,
    branch: &str,
    scratch_root: impl AsRef<Path>,
    credential_helper: Option<&str>,
) -> anyhow::Result<RoundWorkspace> {
    std::fs::create_dir_all(scratch_root.as_ref())?;
    let dir = tempfile::Builder::new()
        .prefix("assembly-round-")
        .tempdir_in(scratch_root)?;

    git::clone_into(remote_url, dir.path(), credential_helper).await?;
    // A tag, or a commit reachable only from the ref the job names, is not
    // guaranteed by a plain clone.
    git::run_allowing_failure(dir.path(), &["fetch", "--quiet", CLONE_REMOTE, &start.name]).await?;
    git::check_out_new_branch(dir.path(), branch, &start.sha).await?;
    git::commit_as_assembly_line(dir.path()).await?;

    Ok(RoundWorkspace {
        dir,
        branch: branch.to_string(),
        started_at: start.sha.clone(),
    })
}
```

and `commit`:

```rust
/// # Errors
///
/// See [`git::commit_all`].
pub async fn commit(ws: &RoundWorkspace, message: &str) -> anyhow::Result<Option<String>> {
    git::commit_all(ws.path(), message).await?;
    match git::head_is_ahead_of(ws.path(), &ws.started_at).await? {
        true => git::head_sha(ws.path()).await.map(Some),
        false => Ok(None),
    }
}
```

`src/payload.rs`: delete `RoundPayload::copy`, `RoundPayload::seed_from`,
`RoundRequest::seed_from`, and their lines in `for_round`.

`src/runner/mod.rs`: delete `RunnerProblem::CopyNeedsLocalRunner` and its
`Display` arm. `reasons_a_container_cannot_run` takes only the URL:

```rust
/// What a repository asks for that no container can give it: a remote that
/// is only a path on the host.
#[must_use]
pub fn reasons_a_container_cannot_run(remote_url: &str) -> Vec<RunnerProblem> {
    is_path_on_this_machine(remote_url)
        .then(|| RunnerProblem::RemoteIsLocalPath {
            url: remote_url.to_string(),
        })
        .into_iter()
        .collect()
}
```

and `secrets_or_reasons_it_cannot_run` loses its `copy: &[String]` parameter
(pass `remote_url` straight to `reasons_a_container_cannot_run`).

`src/lifecycle.rs`: drop `&config.copy` from both
`secrets_or_reasons_it_cannot_run` calls and `seed_from` from the
`RoundRequest` literal. `src/round.rs`: drop `&payload.seed_from` and
`&payload.copy` from the `workspace::create` call, and `copy`/`seed_from`
from the unit test's payload literal (and its `PathBuf` import).

- [ ] **Step 6: Delete the tests of what is gone, and fix the callers**

- `tests/git.rs`: delete `seeded_files_stay_on_disk_but_out_of_the_commit`,
  `a_commit_containing_only_seeded_files_is_no_commit_at_all`,
  `a_secret_committed_by_the_agent_fails_the_round`,
  `a_secret_the_agent_committed_and_then_removed_still_fails_the_round`,
  `a_secret_committed_on_a_merged_side_branch_still_fails_the_round`,
  `a_secret_introduced_by_a_merge_commit_itself_fails_the_round`,
  `a_secret_merged_in_and_out_under_the_agents_config_still_fails_the_round`,
  `a_secret_in_an_orphan_root_commit_still_fails_the_round`,
  `a_secret_behind_a_replace_ref_still_fails_the_round`, and
  `commit_all_except` from the import list (add `commit_all` if a remaining
  test uses it; otherwise nothing).
- `tests/support/mod.rs`: `commit_all` becomes
  `git::commit_all(repo, message).await`.
- `tests/workspace.rs`: `Fixture::workspace` takes no argument and calls
  `workspace::create(self.url(), &self.main().await, "al/job-1", self.scratch(), None)`;
  every `fx.workspace(&[])` becomes `fx.workspace()`. Delete
  `seeded_files_are_copied_in_and_kept_out_of_the_commit`,
  `seeding_preserves_nested_paths` and
  `a_missing_seed_path_names_the_file_and_leaves_no_checkout`.
- `tests/round.rs`: delete `seeded_files_reach_the_agent_but_never_the_branch`.
  In `an_agent_that_switches_branches_publishes_what_it_left_checked_out`,
  drop `copy = [\".env\"]\n` from the config, the `.env` write and the
  `history` assertion that follows the `agent-output.txt` one.
- `tests/runner.rs`: delete `a_repository_that_declares_copy_cannot_run_in_a_container`.
  Every `reasons_a_container_cannot_run(&[..], url)` becomes
  `reasons_a_container_cannot_run(url)`; `every_container_problem_is_reported_at_once`
  now expects only `[RunnerProblem::RemoteIsLocalPath { url: "/tmp/origin.git".into() }]`.
  Drop the `&[".env".into()]` / `&[]` argument from every
  `secrets_or_reasons_it_cannot_run` call, and `RunnerProblem::CopyNeedsLocalRunner`
  from `every_reason_a_container_runner_cannot_run_is_reported_at_once`'s
  expectation and from `every_runner_problem_says_what_to_do_about_it`.
  Drop `copy` and `seed_from` from `payload_cloning`.
- `tests/payload.rs`: drop `copy = [\".env\"]\n` from `RUNNABLE`, the
  `payload.copy` assertion, and `seed_from` from `request`.
- `tests/kubernetes_runner.rs`: drop `copy` and `seed_from` from its payload literal.
- `tests/repo_config.rs`: drop `copy = [".env"]` from the config and the
  `config.copy` assertion.
- `tests/cli.rs`: delete `repo_running_with_copy`;
  `docker_preflight_reports_every_problem_before_allocating` uses
  `repo_running("fake-agent.sh")` and loses its `declares \`copy\`` assertion.

- [ ] **Step 7: Run the gate**

Run: `just check`
Expected: PASS. The count drops by the fifteen tests deleted above and rises
by one.

- [ ] **Step 8: Commit**

```bash
jj describe -m "refactor!: delete copy

Nothing on the factory's path has a checkout to copy from. The field,
seeding, the never-commit unstaging and its history check, and the
container refusal go; a config that still declares copy is refused with
what to do instead. Dissolves accepted risk 6.

Tests: <count>."
jj new
```

---

### Task 2: A job's id is claimed by creating its branch on the remote

A job's id stops being "one past every local directory and remote branch" and
becomes "whichever `al/job-N` this process managed to create". Creation is a
push that the remote rejects if the ref exists
(`--force-with-lease=refs/heads/al/job-N:` — an empty expected value means
*must not exist*), so two claimers can never share an id. The branch exists
from the start, at the base commit: a round that changes nothing leaves it
there rather than leaving no branch.

The claim needs a local repository holding the base commit to push from. In
this task that is the user's repository — `git::pinned` has already fetched
the base into it; Task 6 claims from the scratch clone and Task 10 from the
daemon's bare cache, both through this same function.

**Files:**
- Create: `src/claim.rs`
- Modify: `src/lib.rs` (`pub mod claim;`), `src/git.rs`, `src/paths.rs`, `src/lifecycle.rs`
- Test: `tests/claim.rs` (new), `tests/paths.rs`, `tests/cli.rs`

**Interfaces:**
- Consumes: `git::remote_branches_matching`, `paths::job_id_past`, `JobId`.
- Produces:
  `git::create_branch_if_absent(repo: impl AsRef<Path>, remote: &str, sha: &str, branch: &str) -> anyhow::Result<bool>` (`true` created, `false` already there);
  `claim::claim_job(repo: &Path, remote: &str, base_sha: &str) -> anyhow::Result<JobId>`;
  `paths::job_id_past(remote_branches: &[String]) -> Option<JobId>` (local ids no longer count).

- [ ] **Step 1: Write the failing tests**

`tests/claim.rs`:

```rust
//! Claiming a job's id by creating its branch on the remote.

use assembly_line::claim::claim_job;
use assembly_line::git;
use assembly_line::job::JobId;

mod support;

/// A repository with `main` published to a bare origin, and its sha.
async fn published() -> (tempfile::TempDir, std::path::PathBuf, String) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    support::init_git_repo(&repo).await;
    support::add_origin(&repo, &tmp.path().join("origin.git")).await;
    support::publish_main(&repo).await;
    let sha = git::head_sha(&repo).await.unwrap();
    (tmp, repo, sha)
}

#[tokio::test]
async fn the_first_claim_on_a_remote_is_job_1_at_the_base() {
    let (tmp, repo, sha) = published().await;

    let id = claim_job(&repo, "origin", &sha).await.unwrap();

    assert_eq!(id, JobId::from(1));
    let origin = tmp.path().join("origin.git");
    assert_eq!(git::sha_at_ref(&origin, "al/job-1").await.unwrap(), sha);
}

/// A plain push of the same commit to a branch already there would report
/// "up to date" and succeed — which is exactly the collision a claim exists
/// to prevent.
#[tokio::test]
async fn a_branch_already_at_the_same_commit_is_not_claimed_twice() {
    let (_tmp, repo, sha) = published().await;
    assert_eq!(claim_job(&repo, "origin", &sha).await.unwrap(), JobId::from(1));

    assert_eq!(claim_job(&repo, "origin", &sha).await.unwrap(), JobId::from(2));
}

#[tokio::test]
async fn claims_racing_on_one_remote_all_get_different_ids() {
    let (_tmp, repo, sha) = published().await;

    let claims = futures_join_all(
        (0..6).map(|_| {
            let (repo, sha) = (repo.clone(), sha.clone());
            tokio::spawn(async move { claim_job(&repo, "origin", &sha).await.unwrap() })
        }),
    )
    .await;

    let mut ids: Vec<u64> = claims.into_iter().map(u64::from).collect();
    ids.sort_unstable();
    assert_eq!(ids, [1, 2, 3, 4, 5, 6]);
}

/// Awaits every handle in order — `futures` is not a dependency, and six
/// handles need no more than this.
async fn futures_join_all(
    handles: impl IntoIterator<Item = tokio::task::JoinHandle<JobId>>,
) -> Vec<JobId> {
    let mut ids = Vec::new();
    for handle in handles {
        ids.push(handle.await.unwrap());
    }
    ids
}

#[tokio::test]
async fn somebody_elses_higher_job_branch_is_claimed_past() {
    let (_tmp, repo, sha) = published().await;
    git::push_head_as(&repo, "origin", "al/job-41").await.unwrap();

    assert_eq!(claim_job(&repo, "origin", &sha).await.unwrap(), JobId::from(42));
}
```

(`futures_join_all` is a test helper, not library code; the `for` loop is
sequential awaiting, which is what it is for.)

In `tests/paths.rs`, delete `allocates_monotonic_job_ids`,
`ignores_non_numeric_directories_when_allocating` and
`the_jobs_directory_and_the_remote_together_decide_the_next_id`, and make
`a_new_job_id_is_past_the_remotes_job_branches_too`,
`a_branch_at_the_last_id_leaves_none_to_allocate` and
`branches_that_are_not_a_jobs_are_ignored_when_allocating` call
`job_id_past(&branches)` with no local ids — for the last-id case:

```rust
#[test]
fn a_branch_at_the_last_id_leaves_none_to_allocate() {
    assert_eq!(job_id_past(&[JobId::from(u64::MAX).branch_name()]), None);
}
```

Add a test that `latest_job_id` still ignores non-numeric directories,
replacing the deleted allocation one:

```rust
#[test]
fn the_latest_job_ignores_directories_that_are_not_a_jobs() {
    let tmp = tempfile::tempdir().unwrap();
    let root = jobs_root(tmp.path());
    create_job(&root, JobId::from(7)).unwrap();
    std::fs::create_dir_all(root.join("notes")).unwrap();

    assert_eq!(latest_job_id(&root).unwrap(), Some(JobId::from(7)));
}
```

- [ ] **Step 2: Run them to make sure they fail**

Run: `cargo test --test claim`
Expected: FAIL to compile — `unresolved import assembly_line::claim`.

- [ ] **Step 3: Implement**

`src/git.rs`, after `push_head_as`:

```rust
/// Create `branch` on `remote` at `sha`, only if `remote` has no such branch.
/// `false` means somebody already has it.
///
/// `--force-with-lease=<ref>:` with nothing after the colon expects the ref
/// not to exist, and the remote checks that as part of the push itself — so
/// of two pushes racing for one name, exactly one succeeds. A plain push
/// would not do: pushing the commit a branch already points at reports "up
/// to date" and succeeds. No hooks run, as for [`push_head_as`].
pub async fn create_branch_if_absent(
    repo: impl AsRef<Path>,
    remote: &str,
    sha: &str,
    branch: &str,
) -> anyhow::Result<bool> {
    let lease = format!("--force-with-lease=refs/heads/{branch}:");
    let refspec = format!("{sha}:refs/heads/{branch}");
    let pushed = run_allowing_failure(
        repo,
        &["-c", "core.hooksPath=/dev/null", "push", "--porcelain", &lease, remote, &refspec],
    )
    .await?;
    match (pushed.succeeded(), pushed.stdout.contains("[rejected]")) {
        (true, _) => Ok(true),
        (false, true) => Ok(false),
        (false, false) => Err(anyhow::anyhow!(
            "creating {branch} on '{remote}' failed (exit {}): {}",
            pushed.exit_code,
            pushed.stderr.trim()
        )),
    }
}
```

`--porcelain` prints `!\trefs/heads/...\t[rejected] (stale info)` for a lost
lease, which is what distinguishes "taken" from "the remote is unreachable".
If the git in use words it differently, match on `pushed.stdout.starts_with('!')`
per line instead — check with the test above, not by assumption.

`src/paths.rs`: `job_id_past` takes only the remote's branches, and
`next_job_id` is deleted (nothing allocates from directories any more):

```rust
/// One past the highest job branch `remote_branches` carries. Branches that
/// are not a job's are ignored. `None` when a branch has already taken the
/// last id there is.
#[must_use]
pub fn job_id_past(remote_branches: &[String]) -> Option<JobId> {
    remote_branches
        .iter()
        .filter_map(|branch| JobId::from_branch_name(branch))
        .max()
        .map_or(0, u64::from)
        .checked_add(1)
        .map(JobId::from)
}
```

`src/claim.rs`:

```rust
//! Claiming a job's id: creating its branch on the remote before anything
//! else happens, so the claim and the work are the same object.

use crate::git;
use crate::job::JobId;
use crate::paths::job_id_past;
use std::path::Path;

/// How many ids a claim tries before giving up. Each loss means another
/// claimer took the id between our listing and our push; losing this many
/// times in a row means something is claiming far faster than we are.
const CLAIM_ATTEMPTS: usize = 16;

/// The next free job id on `remote`, claimed by creating its branch at
/// `base_sha`. `repo` must hold `base_sha`: the push sends it from there.
///
/// A loop, not a fold: each attempt is a listing and a push, and the first
/// push that creates its branch ends it.
///
/// # Errors
///
/// When the remote cannot be listed or pushed to, when a branch has taken
/// the last id there is, or when every attempt lost its race.
pub async fn claim_job(repo: &Path, remote: &str, base_sha: &str) -> anyhow::Result<JobId> {
    for _ in 0..CLAIM_ATTEMPTS {
        let taken = git::remote_branches_matching(repo, remote, JobId::BRANCH_PATTERN).await?;
        let id = job_id_past(&taken).ok_or_else(|| {
            anyhow::anyhow!(
                "a job branch has taken the last job id — delete {} from the remote",
                JobId::from(u64::MAX).branch_name()
            )
        })?;
        if git::create_branch_if_absent(repo, remote, base_sha, &id.branch_name()).await? {
            return Ok(id);
        }
    }
    Err(anyhow::anyhow!(
        "could not claim a job id on '{remote}': {CLAIM_ATTEMPTS} attempts in a row lost \
         to another claimer — try again"
    ))
}
```

`src/lifecycle.rs`: `Destination::NewJob` carries nothing — delete
`remote_job_branches` from it and from `prepare_start` (and the
`remote_branches_matching` call there). `allocate_job` claims:

```rust
/// A new job's directory, its `meta.json` and its open event log, under the
/// id it claimed on the remote.
///
/// Called only once the round is known good, so a repository that has not
/// opted in leaves no litter and burns no id.
async fn allocate_job(meta: &JobMeta, base_sha: &str) -> anyhow::Result<(JobPaths, EventLog)> {
    let id = claim::claim_job(&meta.repo, DEFAULT_REMOTE, base_sha).await?;
    let paths = paths::create_job(&paths::jobs_root(&meta.repo), id)
        .map_err(|e| anyhow!("preparing the job directory: {e}"))?;

    paths::write_meta(&paths, meta).map_err(|e| anyhow!("writing meta.json: {e}"))?;

    EventLog::open_append(paths.events())
        .map(|log| (paths, log))
        .map_err(|e| anyhow!("opening the event log: {e}"))
}
```

and `run` calls it as `allocate_job(&meta, &start.sha).await.map(|(paths, log)| (paths, log, 1))?`
— bind `let base_sha = start.sha.clone();` before `start` moves into the payload.

`job_branch_tip`'s "its first round committed nothing" branch can no longer
happen for a job claimed by this code, but can for a job made before it;
leave it.

- [ ] **Step 4: Fix the CLI test that assumed a no-op round leaves no branch**

`tests/cli.rs`: `revising_a_job_that_left_no_branch_says_there_is_nothing_to_revise`
now describes the opposite — replace it:

```rust
/// A job's branch is claimed before its round, so even a round that changed
/// nothing leaves one to revise.
#[tokio::test]
async fn a_job_whose_round_changed_nothing_can_still_be_revised() {
    let tmp = repo_running("noop-agent.sh").await;
    assembly(&tmp)
        .args(["run", "--prompt", "x"])
        .assert()
        .success();
    assert_eq!(
        git_on_origin(&tmp, &["rev-parse", "al/job-1"]),
        git(&tmp, &["rev-parse", "main"]),
        "the claimed branch should sit at the base"
    );

    assembly(&tmp)
        .args(["revise", "1", "try again"])
        .assert()
        .success();

    discard_origin(&tmp);
}
```

`a_job_id_already_taken_on_the_remote_is_skipped` keeps passing unchanged:
that is the point of this task, end to end.

- [ ] **Step 5: Run the gate**

Run: `just check`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
jj describe -m "feat(claim): a job's id is claimed by creating its branch on the remote

The id is whichever al/job-N this process created, with a push the
remote rejects if the branch exists, so two claimers never share one.
The branch exists from the start, at the base; local job directories no
longer decide ids.

Tests: <count>."
jj new
```

---

### Task 3: A round's commit carries its whole prompt

Decision 7: the commit subject stays `job 7: <first line>`; the body is the
whole prompt, so an agent revising a job can read what earlier rounds were
asked with `git log`.

**Files:**
- Modify: `src/payload.rs` (`commit_message`)
- Test: `tests/payload.rs`, `tests/round.rs`

**Interfaces:**
- Produces: `payload::commit_message(job_id: JobId, prompt: &str) -> String`, now `pub`.

- [ ] **Step 1: Write the failing tests**

`tests/payload.rs`:

```rust
#[test]
fn a_commit_message_is_the_first_line_then_the_whole_prompt() {
    let prompt = "Add auth\n\nUse sessions, not JWTs.\n";

    assert_eq!(
        commit_message(JobId::from(7), prompt),
        "job 7: Add auth\n\nAdd auth\n\nUse sessions, not JWTs."
    );
}

#[test]
fn a_blank_prompt_still_makes_a_commit_message() {
    assert_eq!(commit_message(JobId::from(7), "  \n"), "job 7: agent work");
}
```

`tests/round.rs`:

```rust
#[tokio::test]
async fn the_branch_remembers_what_its_round_was_asked() {
    let h = Harness::new().await;
    let prompt = "write a file\n\nin the root, please";

    let outcome = h.run_job(prompt).await;
    assert!(outcome.passed);

    let body = git::run_allowing_failure(
        &h.origin,
        &["log", "-1", "--format=%B", &outcome.job_id.branch_name()],
    )
    .await
    .unwrap()
    .stdout;
    assert!(body.contains("in the root, please"), "{body}");
}
```

- [ ] **Step 2: Run them to make sure they fail**

Run: `cargo test --test payload commit_message`
Expected: FAIL to compile — `commit_message` is private.

- [ ] **Step 3: Implement**

```rust
/// A commit message a human can scan in `git log` and an agent can learn
/// from: the job and the first non-blank line of what it was asked, then
/// the whole of what it was asked.
#[must_use]
pub fn commit_message(job_id: JobId, prompt: &str) -> String {
    match prompt.lines().find(|line| !line.trim().is_empty()) {
        Some(first) => format!("job {job_id}: {}\n\n{}", first.trim(), prompt.trim()),
        None => format!("job {job_id}: agent work"),
    }
}
```

The existing `a_payload_carries_everything_the_round_needs_already_resolved`
asserts `commit_message` — update its expected string to the new shape.

- [ ] **Step 4: Run the gate**

Run: `just check`
Expected: PASS. `a_pull_request_is_titled_and_described_from_the_job_not_from_local_commits`
in `tests/delivery.rs` passes the payload's `commit_message` as the pull
request's title; if it now fails because the title carries the body, make the
title the message's first line — `payload.commit_message.lines().next().unwrap_or_default()`
in `lifecycle::run` — and keep the test's expectation.

- [ ] **Step 5: Commit**

```bash
jj describe -m "feat(round): a round's commit carries its whole prompt

The subject stays job N: <first line>; the body is the prompt, so an
agent revising a job can read what earlier rounds were asked.

Tests: <count>."
jj new
```

---

### Task 4: Job state lives under a state root, not in the repository

Job state moves from `<repo>/.assembly/jobs/<id>/` to
`<root>/jobs/<host>/<owner>/<name>/<id>/`, where the root is `--root`, else
`$ASSEMBLY_ROOT`, else `$XDG_STATE_HOME/assembly-line`, else
`~/.local/state/assembly-line`. A repository's place under the root is its
`RepoKey` (P4), derived from its remote URL, because from Task 10 on the
daemon knows a repository only by URL. `meta.json` is replaced by a
`RoundRequested` event (P3): every round asked for — the first and every
revise — records its remote, base, prompt and provider in the log, so a
job's identity is part of the fold rather than beside it.

After this task the target repository's working tree is untouched by a job
in every way, `.assembly/` included.

**Files:**
- Create: `src/locate.rs`
- Modify: `Cargo.toml` (clap `env` feature), `src/lib.rs`, `src/cli.rs`, `src/main.rs`, `src/paths.rs`, `src/event.rs`, `src/report.rs`, `src/lifecycle.rs`, `src/config.rs` (doc only)
- Test: `tests/paths.rs`, `tests/report.rs`, `tests/lifecycle.rs`, `tests/cli.rs`, `tests/delivery.rs`, `tests/support/mod.rs`

**Interfaces:**
- Consumes: `payload::https_equivalent`, `git::split_at_host_colon`, `payload::remote_to_clone`.
- Produces:
  - `paths::state_root(named: Option<PathBuf>, env: impl Fn(&str) -> Option<String>) -> anyhow::Result<PathBuf>`
  - `paths::RepoKey` — `from_remote_url(url: &str) -> Result<RepoKey, UnkeyableRemote>`, `jobs_dir(&self, root: &Path) -> PathBuf`, `repo_cache(&self, root: &Path) -> PathBuf`, `Display` (`github.com/o/r`)
  - `paths::UnkeyableRemote { url: String }` (`Display`, `std::error::Error`)
  - `paths::create_job(jobs_dir: &Path, id: JobId)`, `paths::open_job(jobs_dir: &Path, id: JobId)`, `paths::latest_job_id(jobs_dir: &Path)`, `paths::existing_job_ids(jobs_dir: &Path) -> io::Result<Vec<JobId>>` (now `pub`)
  - `event::EventKind::RoundRequested { remote_url: String, base: PinnedRef, prompt: String, provider: String }`
  - `JobReport` gains `remote_url: Option<String>`, `base: Option<PinnedRef>`, `first_prompt: Option<String>`, `provider: Option<String>`
  - `locate::jobs_dir_of(root: &Path, repo: Option<PathBuf>) -> anyhow::Result<(PathBuf, String)>` (the jobs directory and the remote URL it was keyed from)
  - `locate::job_at(root: &Path, repo: Option<PathBuf>, job_id: Option<u64>) -> anyhow::Result<JobPaths>`
  - `lifecycle::prepare_start(runner, pass_env, root: &Path, request)`, `lifecycle::prepare_revision(runner, pass_env, root: &Path, request)`, `lifecycle::report_for_job(root: &Path, job_id, repo)` and `lifecycle::output_log_of(root: &Path, job_id, repo)` — the last two now `async`
  - `cli::Cli::root: Option<PathBuf>` (`--root`, global, `env = "ASSEMBLY_ROOT"`)
- Deleted: `paths::jobs_root`, `paths::JobMeta`, `paths::write_meta`, `paths::read_meta`.

- [ ] **Step 1: Write the failing tests for the root and the key**

`tests/paths.rs` — delete `meta_round_trips` and
`meta_written_by_an_older_version_still_loads`; replace every
`jobs_root(tmp.path())` with `tmp.path().join("jobs")`; then add:

```rust
use assembly_line::paths::{RepoKey, state_root};
use std::path::{Path, PathBuf};

fn key(url: &str) -> String {
    RepoKey::from_remote_url(url).unwrap().to_string()
}

#[test]
fn a_hosted_repository_is_keyed_by_host_owner_and_name() {
    assert_eq!(key("https://github.com/o/r.git"), "github.com/o/r");
    assert_eq!(key("https://github.com/o/r"), "github.com/o/r");
    assert_eq!(key("https://github.com/o/r/"), "github.com/o/r");
}

/// Review focus 2: one repository, spelled two ways, is one set of jobs.
#[test]
fn ssh_and_https_spellings_of_one_repository_share_a_key() {
    for url in [
        "git@github.com:o/r.git",
        "ssh://git@github.com/o/r.git",
        "ssh://git@github.com:22/o/r.git",
        "https://GitHub.com/o/r.git",
    ] {
        assert_eq!(key(url), "github.com/o/r", "{url}");
    }
}

#[test]
fn a_repository_on_this_machine_is_keyed_under_local() {
    assert_eq!(key("/srv/git/origin.git"), "local/srv/git/origin");
    assert_eq!(key("file:///srv/git/origin.git"), "local/srv/git/origin");
}

#[test]
fn a_url_that_cannot_name_a_directory_is_refused_by_name() {
    for url in ["../origin", "https://github.com/o/../r", "https://github.com/o/r r", ""] {
        let err = RepoKey::from_remote_url(url).unwrap_err();
        assert!(err.to_string().contains(url), "{url}: {err}");
    }
}

#[test]
fn a_keys_jobs_and_cache_live_under_the_root() {
    let key = RepoKey::from_remote_url("git@github.com:o/r.git").unwrap();

    assert_eq!(
        key.jobs_dir(Path::new("/state")),
        PathBuf::from("/state/jobs/github.com/o/r")
    );
    assert_eq!(
        key.repo_cache(Path::new("/state")),
        PathBuf::from("/state/repos/github.com/o/r.git")
    );
}

fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let pairs: Vec<(String, String)> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    move |name| pairs.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
}

#[test]
fn a_named_root_wins_over_the_environment() {
    let root = state_root(Some("/named".into()), env_of(&[("HOME", "/home/u")])).unwrap();
    assert_eq!(root, PathBuf::from("/named"));
}

#[test]
fn the_default_root_follows_xdg_then_home() {
    assert_eq!(
        state_root(None, env_of(&[("XDG_STATE_HOME", "/xdg"), ("HOME", "/home/u")])).unwrap(),
        PathBuf::from("/xdg/assembly-line")
    );
    assert_eq!(
        state_root(None, env_of(&[("HOME", "/home/u")])).unwrap(),
        PathBuf::from("/home/u/.local/state/assembly-line")
    );
}

#[test]
fn with_no_home_the_root_must_be_named() {
    let err = state_root(None, env_of(&[])).unwrap_err();
    assert!(err.to_string().contains("--root"), "{err}");
}
```

`ASSEMBLY_ROOT` is not looked up by `state_root`: clap's `env` feature reads
it into `--root` before `state_root` is called.

- [ ] **Step 2: Run them to make sure they fail**

Run: `cargo test --test paths`
Expected: FAIL to compile — `no RepoKey in paths`.

- [ ] **Step 3: Implement the root and the key in `src/paths.rs`**

Delete `jobs_root`, `next_job_id` (already gone), `JobMeta`, `write_meta`,
`read_meta`, and the `serde` import if nothing else uses it. Make
`existing_job_ids` `pub`. Add:

```rust
/// Where job state lives when `--root` names nowhere: the XDG state
/// directory, or its conventional place under `$HOME`.
///
/// # Errors
///
/// When nothing is named and there is no `$HOME` to default under.
pub fn state_root(
    named: Option<PathBuf>,
    env: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<PathBuf> {
    match (named, env("XDG_STATE_HOME"), env("HOME")) {
        (Some(named), _, _) => Ok(named),
        (None, Some(xdg), _) if !xdg.is_empty() => Ok(PathBuf::from(xdg).join("assembly-line")),
        (None, _, Some(home)) if !home.is_empty() => {
            Ok(PathBuf::from(home).join(".local/state/assembly-line"))
        }
        (None, _, _) => Err(anyhow::anyhow!(
            "no $HOME to keep job state under — name a directory with --root or $ASSEMBLY_ROOT"
        )),
    }
}

/// A repository's place under the state root: its host, then the path
/// segments of its remote URL, from the URL's HTTPS form — so the SSH and
/// HTTPS spellings of one repository are one key. A repository on this
/// machine has the host `local`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RepoKey {
    segments: Vec<String>,
}

/// A remote URL that names no directory a key could be made of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnkeyableRemote {
    pub url: String,
}

impl std::fmt::Display for UnkeyableRemote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the remote '{}' does not name a repository assembly-line can keep state for — \
             use an absolute path, or a URL whose segments are letters, digits, '.', '_' and '-'",
            self.url
        )
    }
}

impl std::error::Error for UnkeyableRemote {}

impl RepoKey {
    /// # Errors
    ///
    /// When a segment is empty, `.` or `..`, or holds anything but ASCII
    /// letters, digits, `.`, `_` and `-` — validated, never sanitized.
    pub fn from_remote_url(url: &str) -> Result<RepoKey, UnkeyableRemote> {
        let https = crate::payload::https_equivalent(url);
        let (host, path) = match https.split_once("://") {
            Some(("file", path)) => ("local".to_string(), path.to_string()),
            Some((_, rest)) => {
                let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
                let host = authority.rsplit('@').next().unwrap_or(authority);
                let host = crate::git::split_at_host_colon(host).map_or(host, |(host, _)| host);
                (host.to_ascii_lowercase(), path.to_string())
            }
            None if https.starts_with('/') => ("local".to_string(), https.clone()),
            None => return Err(UnkeyableRemote { url: url.to_string() }),
        };
        let segments: Vec<String> = std::iter::once(host)
            .chain(
                path.trim_end_matches('/')
                    .trim_end_matches(".git")
                    .split('/')
                    .filter(|segment| !segment.is_empty())
                    .map(str::to_string),
            )
            .collect();

        match segments.len() > 1 && segments.iter().all(|s| is_keyable_segment(s)) {
            true => Ok(RepoKey { segments }),
            false => Err(UnkeyableRemote { url: url.to_string() }),
        }
    }

    /// Where this repository's jobs live under `root`.
    #[must_use]
    pub fn jobs_dir(&self, root: &Path) -> PathBuf {
        self.segments
            .iter()
            .fold(root.join("jobs"), |dir, segment| dir.join(segment))
    }

    /// The daemon's bare repository for this remote, under `root`.
    #[must_use]
    pub fn repo_cache(&self, root: &Path) -> PathBuf {
        let (name, parents) = self
            .segments
            .split_last()
            .expect("a key has a host and at least one path segment");
        parents
            .iter()
            .fold(root.join("repos"), |dir, segment| dir.join(segment))
            .join(format!("{name}.git"))
    }
}

impl std::fmt::Display for RepoKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.segments.join("/"))
    }
}

fn is_keyable_segment(segment: &str) -> bool {
    !matches!(segment, "" | "." | "..")
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}
```

`git::split_at_host_colon` is `pub(crate)` or `pub` already (it is used by
`payload.rs`); if it is private to `git`, make it `pub(crate)`.

The `create_job`, `open_job` and `latest_job_id` parameters are renamed
`jobs_dir` — their bodies do not change.

- [ ] **Step 4: Run them to make sure they pass**

Run: `cargo test --test paths`
Expected: PASS.

- [ ] **Step 5: Write the failing fold test**

`tests/report.rs`:

```rust
#[test]
fn a_jobs_identity_is_its_first_requested_round() {
    let requested = |prompt: &str, sha: &str| EventKind::RoundRequested {
        remote_url: "git@github.com:o/r.git".into(),
        base: PinnedRef { name: "main".into(), sha: sha.into() },
        prompt: prompt.into(),
        provider: "claude".into(),
    };
    let events = timeline(vec![
        (0, requested("add auth", "a1")),
        (1, requested("use sessions", "b2")),
    ]);

    let report = JobReport::from_events(1, &events);

    assert_eq!(report.remote_url.as_deref(), Some("git@github.com:o/r.git"));
    assert_eq!(report.first_prompt.as_deref(), Some("add auth"));
    assert_eq!(report.base.map(|base| base.sha).as_deref(), Some("b2"));
    assert_eq!(report.provider.as_deref(), Some("claude"));
}
```

`timeline` is the file's existing helper that stamps each `EventKind` with a
time so many seconds in. Import `PinnedRef` from `assembly_line::git`.

- [ ] **Step 6: Implement the event and the fold**

`src/event.rs`, first variant of `EventKind`; and replace the enum's doc
paragraph about `meta.json`:

```rust
/// Everything that happens to one job.
///
/// A job's identity — its repository, base, prompt and provider — is its
/// first [`EventKind::RoundRequested`].
```

```rust
    /// A round was asked for: the job's first, or a revise. The first one is
    /// the job's identity.
    RoundRequested {
        remote_url: String,
        /// The base as it was when the round was asked for. A revise
        /// re-pins it at the remote's tip then.
        base: crate::git::PinnedRef,
        prompt: String,
        provider: String,
    },
```

`src/report.rs`: add the four fields to `JobReport` and to `JobProgress`
(with the same names), fold them, and pass them through in `from_events`:

```rust
    /// Where the job clones from and pushes to, from its first request.
    pub remote_url: Option<String>,
    /// The base its latest round was asked to start from.
    pub base: Option<PinnedRef>,
    /// What the job was first asked to do.
    pub first_prompt: Option<String>,
    /// The provider its latest round was asked to use.
    pub provider: Option<String>,
```

```rust
            EventKind::RoundRequested {
                remote_url,
                base,
                prompt,
                provider,
            } => JobProgress {
                remote_url: self.remote_url.or_else(|| Some(remote_url.clone())),
                first_prompt: self.first_prompt.or_else(|| Some(prompt.clone())),
                base: Some(base.clone()),
                provider: Some(provider.clone()),
                ..self
            },
```

- [ ] **Step 7: Add `src/locate.rs`**

```rust
//! From what a person typed — a `--repo`, a job id, or nothing — to where
//! that job's state lives under the root.

use crate::job::JobId;
use crate::paths::{self, JobPaths, RepoKey};
use crate::payload::remote_to_clone;
use crate::workspace::DEFAULT_REMOTE;
use anyhow::anyhow;
use std::path::{Path, PathBuf};

/// The jobs directory for `repo` — or for the repository the user is
/// standing in — under `root`, and the remote URL it is keyed by.
///
/// # Errors
///
/// When there is no repository here, it has no remote, or the remote names
/// no directory a key could be made of.
pub async fn jobs_dir_of(root: &Path, repo: Option<PathBuf>) -> anyhow::Result<(PathBuf, String)> {
    let checkout = match repo {
        Some(named) => named,
        None => enclosing_checkout()?,
    };
    let remote_url = remote_to_clone(&checkout, DEFAULT_REMOTE).await?;
    let key = RepoKey::from_remote_url(&remote_url)?;
    Ok((key.jobs_dir(root), remote_url))
}

/// Job `job_id` of `repo` under `root`, or its latest when `job_id` is
/// `None`.
///
/// # Errors
///
/// As [`jobs_dir_of`]; and when there are no jobs yet, or no such job.
pub async fn job_at(
    root: &Path,
    repo: Option<PathBuf>,
    job_id: Option<u64>,
) -> anyhow::Result<JobPaths> {
    let (jobs_dir, _) = jobs_dir_of(root, repo).await?;
    let id = match job_id {
        Some(id) => JobId::from(id),
        None => paths::latest_job_id(&jobs_dir)?.ok_or_else(|| anyhow!("no jobs yet"))?,
    };
    Ok(paths::open_job(&jobs_dir, id)?)
}

fn enclosing_checkout() -> anyhow::Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    paths::git_root(&cwd).ok_or_else(|| {
        anyhow!("not inside a git repository — name one with --repo")
    })
}
```

- [ ] **Step 8: Move `lifecycle` onto the root**

In `src/lifecycle.rs`:

- `prepare_start` and `prepare_revision` take `root: &Path` after
  `pass_env`. `Destination::NewJob` carries `jobs_dir: PathBuf`, computed in
  `prepare_start` as `RepoKey::from_remote_url(&located.remote_url)` →
  `.jobs_dir(root)`; a key error is `Refusal::Unpreparable`.
- `ReadyRound` loses `meta: JobMeta` and gains `repo: PathBuf`,
  `base_ref: String`, `provider: String` and `original_prompt: String`
  (what `meta` carried); every use of `meta.x` becomes the field.
- `allocate_job(jobs_dir: &Path, repo: &Path, base_sha: &str)` claims as in
  Task 2, creates the directory under `jobs_dir`, and opens the log — no
  `meta.json`.
- `run` appends `RoundRequested { remote_url, base: start.clone(), prompt:
  round_prompt.clone(), provider }` to the job's log before building the
  payload — for a new job and for a revise alike. For a revise, `base` is
  the job's base re-pinned now (`locate_revision`'s `base`); keep it in
  `RevisionLocated` and carry it to `ReadyRound` as `requested_base`, since
  `start` there is the job branch's tip. For a new job `requested_base` is
  `start`.
- `locate_revision(root, job_id, repo)` finds the job with
  `locate::job_at(root, repo.clone(), Some(job_id))`, reads its events,
  folds them, and takes `base_ref` from `report.base`, `provider` from
  `report.provider`, and the original prompt from `report.first_prompt` —
  any of them missing is `anyhow!("job {job_id} has no recorded request —
  it was made by an older assembly-line")`. The checkout it runs git in is
  `repo`, or the enclosing one.
- `report_for_job(root, job_id, repo)` and `output_log_of(root, job_id, repo)`
  become `async`, find the job with `locate::job_at`, and otherwise do what
  they did. Delete `locate_job`, `repository_named_or_enclosing` and
  `enclosing_repo_root` (their work is in `locate` now).
- `RoundConclusion::to_lines` keeps its `state: <dir>` line — the directory
  is under the root now.

- [ ] **Step 9: Thread the root through the CLI**

`Cargo.toml`: `clap = { version = "4", features = ["derive", "env"] }`.

`src/cli.rs`:

```rust
pub struct Cli {
    /// Where job state lives. Defaults to $XDG_STATE_HOME/assembly-line,
    /// or ~/.local/state/assembly-line.
    #[arg(long, global = true, env = "ASSEMBLY_ROOT")]
    pub root: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}
```

The `--repo` help on `status`, `logs` and `revise` says "The repository whose
jobs to look at. Defaults to the enclosing one."

`src/main.rs`: `let cli = Cli::parse();` then resolve the root once, before
the `match`, as a usage error when it fails:

```rust
    let cli = Cli::parse();
    let root = match paths::state_root(cli.root, |name| std::env::var(name).ok()) {
        Ok(root) => root,
        Err(e) => return fail_with_usage_error(e),
    };
```

and pass `&root` to `prepare_start`, `prepare_revision`, `report_for_job`
and `output_log_of`. `print_job_status` and `print_job_log` run in the async
runtime now (`in_async_runtime`), since locating a job reads the remote URL.

- [ ] **Step 10: Move the tests onto the root**

- `tests/support/mod.rs`: `Harness` gains `pub root: PathBuf`
  (`tmp.path().join("root")`) and passes `&self.root` to `prepare_start` and
  `prepare_revision`. Delete the `.git/info/exclude` write in `with_config`
  and its comment — nothing lands in the repository now. `job_paths` becomes
  `paths::create_job(&self.tmp.path().join("jobs"), THE_JOB.into())`.
- `tests/lifecycle.rs`: `job_in(h, id)` creates the job under
  `RepoKey::from_remote_url(h.origin.to_str().unwrap()).unwrap().jobs_dir(&h.root)`
  and appends a `RoundRequested` (remote `h.origin`, base `main` at
  `git::sha_at_ref(&h.origin, "main")`, prompt `"x"`, provider `"fake"`)
  instead of writing `meta.json`. Every `prepare_*` call gains `&h.root`;
  `report_for_job` / `output_log_of` calls gain `&h.root` and `.await`.
  `a_revise_is_numbered_past_the_highest_round_recorded` opens the job with
  `locate::job_at(&h.root, Some(h.repo.clone()), Some(first.job_id.into()))`.
- `tests/cli.rs`: add beside `origin_for`:

```rust
/// Where this test's job state lives: outside the repository, which a job
/// must leave untouched.
fn root_for(tmp: &tempfile::TempDir) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "assembly-test-root-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    ))
}

/// Job `id`'s directory under this test's root.
fn job_dir(tmp: &tempfile::TempDir, id: u64) -> std::path::PathBuf {
    RepoKey::from_remote_url(origin_for(tmp).to_str().unwrap())
        .unwrap()
        .jobs_dir(&root_for(tmp))
        .join(id.to_string())
}
```

  `assembly(tmp)` sets `.env("ASSEMBLY_ROOT", root_for(tmp))`. Rename
  `discard_origin` to `discard_outside_state` and have it remove both
  `origin_for(tmp)` and `root_for(tmp)`; update every call. Every
  `tmp.path().join(".assembly/jobs/N/...")` becomes `job_dir(&tmp, N).join("...")`;
  the `meta.json` assertion in `run_exits_zero_and_records_the_job` is
  deleted. `a_job_started_elsewhere_is_found_by_pointing_the_read_commands_at_it`
  asserts `job_dir(&target, 1).join("events.jsonl").is_file()` and keeps
  `!standing_in.path().join(".assembly").exists()`; its `status` without
  `--repo` from `standing_in` now fails because `standing_in` has no
  `origin` — expect `.stderr(contains("no 'origin' remote"))` instead of
  "no jobs yet". `status_with_no_jobs_explains_itself` and
  `logs_for_an_unknown_job_explains_itself` give their repository an origin
  first (`publish_to_origin(&tmp).await`) so they reach the check they test.
  `run_outside_a_git_repo_explains_itself` expects `"not inside a git repository"`
  unchanged (the message's tail changed, not its head).
- Replace `a_job_writes_nothing_outside_dot_assembly_in_the_target_repository`:

```rust
#[tokio::test]
async fn a_job_writes_nothing_to_the_target_repositorys_working_tree() {
    let tmp = repo_running("fake-agent.sh").await;
    let before = git(&tmp, &["rev-parse", "HEAD"]);

    assembly(&tmp)
        .args(["run", "--prompt", "x"])
        .assert()
        .success();

    assert_eq!(before, git(&tmp, &["rev-parse", "HEAD"]), "HEAD moved");
    // Full porcelain, untracked files included: nothing a job does may show
    // up here, `.assembly/` included — its state lives under the root.
    assert_eq!(git(&tmp, &["status", "--porcelain"]), "");
    assert!(job_dir(&tmp, 1).join("events.jsonl").is_file());

    discard_outside_state(&tmp);
}
```

- `tests/delivery.rs`: its own `assembly(tmp)` sets `ASSEMBLY_ROOT` the same
  way, with its own `root_for`, removed in its own `discard_origin`.
- `src/config.rs`: delete the `REPO_CONFIG_PATH` doc paragraph about
  ignoring `.assembly/jobs/`.

- [ ] **Step 11: Run the gate**

Run: `just check`
Expected: PASS.

- [ ] **Step 12: Commit**

```bash
jj describe -m "refactor!: job state lives under a state root, not in the repository

<root>/jobs/<host>/<owner>/<name>/<id>, where the root is --root,
\$ASSEMBLY_ROOT, or the XDG state directory. A repository is keyed by
its remote URL's HTTPS form, so its SSH and HTTPS spellings share jobs.
meta.json is replaced by a RoundRequested event per round asked for.
A job no longer writes anything into the target repository.

Tests: <count>."
jj new
```

---

### Task 5: `submit` hands a job to a runner; a revise is `submit --job`

A pure rename that frees the name `run` for Task 6. The command that
orchestrates a round on a runner — today's `run` and `revise` — becomes
`submit`, and a revise is `submit --job N --prompt <feedback>`. Its
behavior is unchanged: it still runs the round in the foreground until
Task 10 moves it into the daemon.

**Files:**
- Modify: `src/cli.rs`, `src/main.rs`, `src/lifecycle.rs`, `justfile` (`demo`)
- Test: `tests/cli.rs`, `tests/delivery.rs`, `tests/lifecycle.rs`, `tests/support/mod.rs`

**Interfaces:**
- Produces:
  - `cli::Command::Submit { prompt, prompt_file, repo, base_ref, provider, job: Option<u64>, runner: RunnerArgs }` (replaces `Run` and `Revise`)
  - `lifecycle::Work { Start(StartRequest), Revise(RevisionRequest) }` and
    `lifecycle::Work::from_submission(prompt, prompt_file, repo, base_ref, provider, job) -> Work`
  - `lifecycle::RevisionRequest { job_id: u64, prompt: Option<String>, prompt_file: Option<PathBuf>, repo: Option<PathBuf> }` (was `feedback: String`)

- [ ] **Step 1: Change the CLI tests to the new spelling (they are the failing tests)**

In `tests/cli.rs` and `tests/delivery.rs`: every `.args(["run", ...])`
becomes `.args(["submit", ...])`, and every
`.args(["revise", "<id>", "<feedback>", rest...])` becomes
`.args(["submit", "--job", "<id>", "--prompt", "<feedback>", rest...])`.
`inapplicable_flags_of` matches `Subcommand::Submit { runner, .. }` only;
`pass_env_is_inapplicable_to_the_local_runner` parses
`["assembly", "submit", "--job", "1", "--prompt", "fb", "--pass-env", "KEY"]`.
Replace `revise_without_feedback_is_a_usage_error` with:

```rust
/// A revise needs to be told what to change, like any submit.
#[tokio::test]
async fn a_revise_without_a_prompt_is_a_usage_error() {
    let tmp = repo_running("fake-agent.sh").await;

    assembly(&tmp).args(["submit", "--job", "1"]).assert().code(2);

    discard_outside_state(&tmp);
}
```

and add:

```rust
/// A revise starts from the job's own base and provider; naming others
/// would say something the revise cannot honour.
#[test]
fn a_revise_cannot_name_a_ref_or_a_provider() {
    for flag in ["--ref", "--provider"] {
        let parsed = Cli::try_parse_from([
            "assembly", "submit", "--job", "1", "--prompt", "fb", flag, "x",
        ]);
        assert!(parsed.is_err(), "{flag} was accepted alongside --job");
    }
}
```

- [ ] **Step 2: Run them to make sure they fail**

Run: `cargo test --test cli`
Expected: FAIL — `unrecognized subcommand 'submit'`.

- [ ] **Step 3: Rename in `src/cli.rs`**

Replace `Run` and `Revise` with:

```rust
    /// Hand a job to a runner: a new job, or with --job, another round of
    /// an existing one
    Submit {
        /// What the agent is asked to do — for a revise, what to change
        #[arg(
            long,
            conflicts_with = "prompt_file",
            required_unless_present = "prompt_file"
        )]
        prompt: Option<String>,
        /// Read the prompt from a file instead
        #[arg(long)]
        prompt_file: Option<PathBuf>,
        /// The repository to work in. Defaults to the enclosing one.
        #[arg(long)]
        repo: Option<PathBuf>,
        /// What to branch from. Defaults to the checked-out branch.
        #[arg(long = "ref", conflicts_with = "job")]
        base_ref: Option<String>,
        /// Overrides the repository's declared provider
        #[arg(long, conflicts_with = "job")]
        provider: Option<String>,
        /// Another round of this job, on its own branch, instead of a new job
        ///
        /// A new round, not a resumption: the agent's prior work arrives as
        /// files on disk, and this round appends to the job's branch.
        #[arg(long)]
        job: Option<u64>,
        #[command(flatten)]
        runner: RunnerArgs,
    },
```

The `RunnerArgs` doc now reads "Where a round runs. A revise is a new round
cut from the job's branch, so it may run somewhere the first round did not."

- [ ] **Step 4: Route it**

`src/lifecycle.rs`: `RevisionRequest` carries `prompt` and `prompt_file`
instead of `feedback`; `prepare_revision` turns them into text with the same
`prompt_text` that `prepare_start` uses (a missing file is
`Refusal::Unpreparable`). Move `Work` out of `main.rs` into `lifecycle.rs`:

```rust
/// What `submit` was asked for: a new job, or another round of one.
#[derive(Debug, Clone)]
pub enum Work {
    Start(StartRequest),
    Revise(RevisionRequest),
}

impl Work {
    /// `submit`'s flags as the work they describe. clap has already kept
    /// `--ref` and `--provider` away from `--job`.
    #[must_use]
    pub fn from_submission(
        prompt: Option<String>,
        prompt_file: Option<PathBuf>,
        repo: Option<PathBuf>,
        base_ref: Option<String>,
        provider: Option<String>,
        job: Option<u64>,
    ) -> Work {
        match job {
            None => Work::Start(StartRequest {
                prompt,
                prompt_file,
                repo,
                base_ref,
                provider,
            }),
            Some(job_id) => Work::Revise(RevisionRequest {
                job_id,
                prompt,
                prompt_file,
                repo,
            }),
        }
    }
}
```

`src/main.rs`: one arm replaces the two:

```rust
        Command::Submit {
            prompt,
            prompt_file,
            repo,
            base_ref,
            provider,
            job,
            runner,
        } => in_async_runtime(run_work_on_chosen_runner(
            &root,
            runner,
            Work::from_submission(prompt, prompt_file, repo, base_ref, provider, job),
        )),
```

and `run_work` matches `lifecycle::Work`. `tests/support/mod.rs`'s
`revise_job` builds `RevisionRequest { job_id, prompt: Some(feedback.into()), prompt_file: None, repo }`;
`tests/lifecycle.rs`'s `RevisionRequest` literals likewise.

`justfile`: `demo` runs `"$bin" submit --prompt "make a change" || true`.

- [ ] **Step 5: Run the gate**

Run: `just check`
Expected: PASS. The count is unchanged but for the one test added
(`a_revise_cannot_name_a_ref_or_a_provider`).

- [ ] **Step 6: Commit**

```bash
jj describe -m "refactor(cli)!: submit hands a job to a runner; a revise is submit --job

run and revise become one command, submit, and --job N makes it another
round of job N. Frees the name run for the command that is one whole job.

Tests: <count>."
jj new
```

---

### Task 6: `assembly run` does one whole job in this process

`run` is the job: it resolves the repository and base, clones into scratch,
reads and validates the config at the base's commit, claims an id (or, with
`--job`, checks out that job's branch), runs the round, pushes, opens the
pull request, and concludes. It keeps no state. With `--frames` its stdout is
NDJSON for a collector; without, a person reads the agent's output live and
the conclusion at the end.

It sits beside `job-exec` in this change: `submit`'s runners still launch
`job-exec` until Task 7 switches them. The round itself is shared —
`round::run_round_in` — so the two paths cannot drift.

`run` numbers no rounds (P1): it emits no `RoundStarted`. `RoundPayload`
still has a `round` field for `job-exec`'s sake; `run` sets it to 0, and
Task 7 deletes the field.

**Files:**
- Create: `src/run.rs`, `tests/run.rs`, `tests/fixtures/forge-token-reporting-agent.sh`
- Modify: `src/lib.rs`, `src/cli.rs`, `src/main.rs`, `src/round.rs`, `src/workspace.rs`, `src/frame.rs`, `src/event.rs`, `src/report.rs`, `src/delivery.rs`, `src/exec.rs`, `src/payload.rs`, `src/lifecycle.rs` (imports `prompt_text` from `run`)
- Test: `tests/frame.rs`, `tests/report.rs`, `tests/workspace.rs`, `tests/delivery.rs`

**Interfaces:**
- Consumes: `claim::claim_job`, `config::RepoConfig`, `payload::RoundPayload::for_round`, `payload::commit_message`, `delivery::deliver`, `git::TOKEN_CREDENTIAL_HELPER`.
- Produces:
  - `run::RunRequest { repo: Option<String>, base_ref: Option<String>, job: Option<u64>, prompt: Option<String>, prompt_file: Option<PathBuf>, provider: Option<String>, provision_toolchain: bool }`
  - `run::RunRefused { ConfigNotRunnable(Vec<ConfigError>), NoJobBranch(JobId), Unpreparable(anyhow::Error) }` with `itemized_reasons()` and `Display`
  - `run::prepare_run(request: RunRequest, scratch_root: &Path, cancel: &CancellationToken) -> Result<ReadyJob, RunRefused>`
  - `run::ReadyJob` — `to_announcement_line(&self) -> String`, `run(self, frames: &FrameWriter<W>, cancel) -> anyhow::Result<JobConclusion>`
  - `run::JobConclusion { job: JobId, verdict: Verdict, report: JobReport, base_differs: Option<BaseDiffers> }` with `to_lines()`
  - `run::prompt_text(prompt: Option<String>, prompt_file: Option<PathBuf>) -> anyhow::Result<String>` (moved from `lifecycle`)
  - `run::BaseDiffers` (moved from `lifecycle`; `lifecycle` imports it)
  - `round::run_round_in(payload, ws: &RoundWorkspace, frames, provisioning_deadline: Instant, cancel) -> anyhow::Result<Verdict>`; `round::PROVISIONING_LIMIT` now `pub`
  - `workspace::ScratchClone` (`path()`), `workspace::clone_scratch(remote_url, scratch_root, credential_helper) -> anyhow::Result<ScratchClone>`,
    `workspace::pin_in_clone(clone: &ScratchClone, git_ref: &str, pinned_sha: Option<&str>) -> anyhow::Result<PinnedRef>`,
    `workspace::job_branch_in_clone(clone: &ScratchClone, branch: &str) -> anyhow::Result<Option<PinnedRef>>`,
    `workspace::start_round(clone: ScratchClone, start: &PinnedRef, branch: &str) -> anyhow::Result<RoundWorkspace>`
  - `frame::FrameWriter::events_so_far(&self) -> Vec<Event>`; `frame::ReadableFrames<W: Write>` (`new(text: W)`)
  - `event::EventKind::PullRequestOpened { url: String }`; `JobReport::pull_request: Option<String>`
  - `delivery::Delivered::AlreadyOpen { url: String }`
  - `payload::FORGE_TOKEN_VAR: &str = "GH_TOKEN"`
  - `cli::Command::Run { prompt, prompt_file, repo: Option<String>, base_ref, job, provider, frames: bool, provision_toolchain: bool }`

- [ ] **Step 1: Write the failing unit-level tests**

`tests/frame.rs`:

```rust
#[test]
fn a_frame_writer_remembers_the_events_it_sent() {
    let frames = FrameWriter::new(Vec::new());
    frames.append_output("hello").unwrap();
    frames.append_event(EventKind::RoundPassed).unwrap();

    let kinds: Vec<EventKind> = frames.events_so_far().into_iter().map(|e| e.kind).collect();
    assert_eq!(kinds, [EventKind::RoundPassed]);
}

#[test]
fn a_person_reads_the_output_and_none_of_the_frames() {
    let frames = FrameWriter::new(ReadableFrames::new(Vec::new()));
    frames.append_output("agent: working").unwrap();
    frames.append_event(EventKind::RoundPassed).unwrap();
    frames.append_output("agent: done").unwrap();

    let text = String::from_utf8(frames.into_sink().unwrap().into_text()).unwrap();
    assert_eq!(text, "agent: working\nagent: done\n");
}
```

(`FrameWriter::into_sink(self) -> Option<W>` unwraps the shared sink once
the writer is the last clone; `ReadableFrames::into_text(self) -> W` gives
back what it wrote to. Both are for tests and the conclusion, and both are
added in Step 3.)

`tests/report.rs`:

```rust
#[test]
fn a_job_reports_the_pull_request_it_opened() {
    let events = timeline(vec![(0, EventKind::PullRequestOpened {
        url: "https://github.com/o/r/pull/7".into(),
    })]);

    let report = JobReport::from_events(7, &events);

    assert_eq!(report.pull_request.as_deref(), Some("https://github.com/o/r/pull/7"));
    assert!(report.to_status_lines().contains(&"pull request: https://github.com/o/r/pull/7".to_string()));
}

/// `run` numbers no rounds, so a report folded from its events alone has
/// seen none — and must not claim round 1 of a job it may be revising.
#[test]
fn a_report_that_saw_no_round_start_names_no_round() {
    let events = timeline(vec![(0, EventKind::RoundPassed)]);

    assert_eq!(JobReport::from_events(3, &events).to_summary_line(), "job 3: passed");
}
```

`tests/workspace.rs`:

```rust
#[tokio::test]
async fn a_pinned_commit_is_used_even_after_its_ref_moves() {
    let fx = Fixture::new().await;
    let pinned = fx.main().await;
    std::fs::write(fx.repo.join("later.txt"), "later\n").unwrap();
    support::commit_all(&fx.repo, "later").await.unwrap().unwrap();
    support::publish_main(&fx.repo).await;

    let clone = workspace::clone_scratch(fx.url(), fx.scratch(), None).await.unwrap();
    let base = workspace::pin_in_clone(&clone, "main", Some(&pinned.sha)).await.unwrap();

    assert_eq!(base, pinned);
}

#[tokio::test]
async fn a_job_branch_the_remote_lacks_is_none_not_an_error() {
    let fx = Fixture::new().await;
    let clone = workspace::clone_scratch(fx.url(), fx.scratch(), None).await.unwrap();

    assert_eq!(workspace::job_branch_in_clone(&clone, "al/job-9").await.unwrap(), None);
}
```

- [ ] **Step 2: Run them to make sure they fail**

Run: `cargo test --test frame --test report --test workspace`
Expected: FAIL to compile.

- [ ] **Step 3: Implement the building blocks**

`src/frame.rs` — `Numbered` gains `events: Vec<Event>`, which `append_event`
pushes to after a successful write:

```rust
    /// Every event this writer has sent, in order — what the round
    /// reported, for a conclusion drawn in the same process.
    ///
    /// # Panics
    ///
    /// Panics if a writer panicked while holding the lock.
    #[must_use]
    pub fn events_so_far(&self) -> Vec<Event> {
        self.shared.lock().expect("frame writer lock").events.clone()
    }

    /// The sink, once this is the last clone of the writer.
    #[must_use]
    pub fn into_sink(self) -> Option<W> {
        Arc::try_unwrap(self.shared)
            .ok()
            .and_then(|numbered| numbered.into_inner().ok())
            .map(|numbered| numbered.sink)
    }
```

and:

```rust
/// A frame sink for a person at a terminal: the text of every output frame,
/// one per line, and nothing of the events — the conclusion reports those.
#[derive(Debug)]
pub struct ReadableFrames<W: Write> {
    text: W,
    partial: Vec<u8>,
}

impl<W: Write> ReadableFrames<W> {
    pub fn new(text: W) -> Self {
        ReadableFrames {
            text,
            partial: Vec::new(),
        }
    }

    pub fn into_text(self) -> W {
        self.text
    }

    fn write_readable(&mut self, line: &[u8]) -> io::Result<()> {
        match serde_json::from_slice::<Frame>(line) {
            Ok(Frame { body: FrameBody::Output(text), .. }) => writeln!(self.text, "{text}"),
            Ok(Frame { body: FrameBody::Event(_), .. }) => Ok(()),
            Err(_) => {
                self.text.write_all(line)?;
                self.text.write_all(b"\n")
            }
        }
    }
}

impl<W: Write> Write for ReadableFrames<W> {
    /// A loop: each complete line is written before the next is looked for,
    /// and a line split across writes waits in `partial`.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.partial.extend_from_slice(buf);
        while let Some(end) = self.partial.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=end).collect();
            self.write_readable(&line[..line.len() - 1])?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.text.flush()
    }
}
```

`src/event.rs`, after `BranchPushed`:

```rust
    /// The job's branch has a pull request, opened by the round that just
    /// passed.
    PullRequestOpened {
        url: String,
    },
```

`src/report.rs`: `JobReport` and `JobProgress` gain
`pull_request: Option<String>`, set by `PullRequestOpened`; `to_status_lines`
appends `format!("pull request: {url}")` after the branch line.
`to_summary_line` omits `round N` when `rounds` is 0 and omits the
parentheses when there are no facts at all:

```rust
    pub fn to_summary_line(&self) -> String {
        let facts: Vec<String> = [
            (self.rounds > 0).then(|| format!("round {}", self.rounds)),
            self.diff.map(|d| d.to_string()),
        ]
        .into_iter()
        .flatten()
        .collect();
        let facts = match facts.is_empty() {
            true => String::new(),
            false => format!(" ({})", facts.join(", ")),
        };

        format!(
            "job {}: {}{facts}{}",
            self.id,
            self.state.label(),
            self.detail
                .as_ref()
                .map(|why| format!(" — {why}"))
                .unwrap_or_default(),
        )
    }
```

and `from_events` no longer floors `rounds` at 1 (`rounds: progress.rounds`).
Update `JobReport::rounds`'s doc: "the highest round any `RoundStarted`
records; 0 when none does." Existing report tests that relied on the floor
of 1 get an explicit `RoundStarted { round: 1 }`.

`src/delivery.rs`: before creating, ask whether the branch already has one:

```rust
    /// The branch already had a pull request — a revise, whose new commits
    /// land on it by themselves.
    AlreadyOpen {
        url: String,
    },
```

`Display`: `Self::AlreadyOpen { url } => write!(f, "updated {url}")`. In
`open_pull_request`, first:

```rust
    let existing = Command::new("gh")
        .args(["pr", "view", job_branch, "--json", "url", "--jq", ".url"])
        .current_dir(repo)
        .output()
        .await;
    if let Ok(out) = &existing
        && out.status.success()
        && !out.stdout.is_empty()
    {
        return Delivered::AlreadyOpen {
            url: String::from_utf8_lossy(&out.stdout).trim().to_string(),
        };
    }
```

(A `gh` that fails `pr view` — no pull request, no `gh`, the refusing fake —
falls through to `pr create` as before.)

`src/payload.rs`:

```rust
/// The forge credential `gh` opens a pull request with. Withheld from the
/// agent's environment like [`GIT_TOKEN_VAR`], and within its reach like it.
pub const FORGE_TOKEN_VAR: &str = "GH_TOKEN";
```

`src/exec.rs`: add `.env_remove(FORGE_TOKEN_VAR)` beside the git token's.

`src/workspace.rs` — split `create` into its steps and keep it as their
composition for `job-exec`:

```rust
/// A scratch clone of the remote, with nothing checked out yet. Deleted
/// when dropped.
#[derive(Debug)]
pub struct ScratchClone {
    dir: tempfile::TempDir,
}

impl ScratchClone {
    #[must_use]
    pub fn path(&self) -> &Path {
        self.dir.path()
    }
}

/// # Errors
///
/// A clone that fails midway leaves nothing behind.
pub async fn clone_scratch(
    remote_url: &str,
    scratch_root: impl AsRef<Path>,
    credential_helper: Option<&str>,
) -> anyhow::Result<ScratchClone> {
    std::fs::create_dir_all(scratch_root.as_ref())?;
    let dir = tempfile::Builder::new()
        .prefix("assembly-round-")
        .tempdir_in(scratch_root)?;
    git::clone_into(remote_url, dir.path(), credential_helper).await?;
    Ok(ScratchClone { dir })
}

/// `git_ref` as the clone's remote has it, pinned to `pinned_sha` when one
/// is given — the commit a daemon validated, which the ref may have moved
/// past since.
///
/// # Errors
///
/// When the remote does not carry `git_ref`, or `pinned_sha` is not a
/// commit the remote can supply.
pub async fn pin_in_clone(
    clone: &ScratchClone,
    git_ref: &str,
    pinned_sha: Option<&str>,
) -> anyhow::Result<PinnedRef> {
    let tip = git::pinned(clone.path(), CLONE_REMOTE, git_ref).await?;
    match pinned_sha {
        None => Ok(tip),
        Some(sha) => {
            if !git::has_commit(clone.path(), sha).await? {
                // A ref that moved on past the pin still has it in its
                // history; one that was rewritten may not, so ask for it.
                git::run_allowing_failure(clone.path(), &["fetch", "--quiet", CLONE_REMOTE, sha])
                    .await?;
            }
            match git::has_commit(clone.path(), sha).await? {
                true => Ok(PinnedRef { name: git_ref.to_string(), sha: sha.to_string() }),
                false => Err(anyhow::anyhow!(
                    "'{sha}' is not a commit '{CLONE_REMOTE}' can supply for '{git_ref}'"
                )),
            }
        }
    }
}

/// The tip of job branch `branch` on the clone's remote, or `None` when the
/// remote has no such branch.
///
/// # Errors
///
/// When the remote cannot be asked.
pub async fn job_branch_in_clone(
    clone: &ScratchClone,
    branch: &str,
) -> anyhow::Result<Option<PinnedRef>> {
    match git::remote_lacks_ref(clone.path(), CLONE_REMOTE, branch).await? {
        true => Ok(None),
        false => git::pinned(clone.path(), CLONE_REMOTE, branch).await.map(Some),
    }
}

/// Check `branch` out at `start` in `clone`, committing as assembly-line,
/// and make it the round's workspace.
///
/// # Errors
///
/// When the checkout or the identity cannot be set.
pub async fn start_round(
    clone: ScratchClone,
    start: &PinnedRef,
    branch: &str,
) -> anyhow::Result<RoundWorkspace> {
    git::check_out_new_branch(clone.path(), branch, &start.sha).await?;
    git::commit_as_assembly_line(clone.path()).await?;
    Ok(RoundWorkspace {
        clone,
        branch: branch.to_string(),
        started_at: start.sha.clone(),
    })
}
```

`RoundWorkspace`'s `dir: TempDir` becomes `clone: ScratchClone`; `path()`
returns `self.clone.path()`, and `discard` closes `ws.clone.dir`. `create`
becomes `clone_scratch` + the start ref's `fetch` + `start_round`.

`src/git.rs`:

```rust
/// Whether `repo` has `sha` as a commit.
pub async fn has_commit(repo: impl AsRef<Path>, sha: &str) -> anyhow::Result<bool> {
    let spec = format!("{sha}^{{commit}}");
    Ok(run_allowing_failure(repo, &["cat-file", "-e", &spec])
        .await?
        .succeeded())
}
```

`src/round.rs` — `PROVISIONING_LIMIT` becomes `pub`. Split `round_result`
so the part after the clone takes a workspace it is handed and does not
discard it:

```rust
/// One round in `ws`, which the caller cloned and will discard: provision,
/// agent, commit, push, `verify`, and the events that record it. No
/// `RoundStarted`: whoever collects the round numbers it.
///
/// # Errors
///
/// As [`run_round`]: only when the round's frames cannot be written.
pub async fn run_round_in<W: Write + Send + 'static>(
    payload: &RoundPayload,
    ws: &RoundWorkspace,
    frames: &FrameWriter<W>,
    provisioning_deadline: Instant,
    cancel: CancellationToken,
) -> anyhow::Result<Verdict> {
    let timeout = payload.command_limit_secs.map(Duration::from_secs);
    let result = result_in_workspace(payload, ws, frames, provisioning_deadline, timeout, cancel)
        .await
        .unwrap_or_else(|e| RoundResult::Failed {
            reason: e.to_string(),
            work: None,
        });
    record_completion(frames, result)
}
```

`result_in_workspace` is the body of today's `round_result` from
`provision_toolchain(...)` to the final `Ok(match ...)`, minus the
`workspace::discard` call. `run_round` (for `job-exec`) keeps its
`RoundStarted` and its clone-under-timeout, then calls `run_round_in`, then
discards and reports a discard failure as an output frame, as today.

- [ ] **Step 4: Run them to make sure they pass**

Run: `cargo test --test frame --test report --test workspace`
Expected: PASS.

- [ ] **Step 5: Write the failing end-to-end tests**

`tests/fixtures/forge-token-reporting-agent.sh`:

```bash
#!/usr/bin/env bash
# Records whether the forge token reached the agent.
echo "${GH_TOKEN:-absent}" > forge-token.txt
```

`tests/run.rs`:

```rust
//! `assembly run`: one whole job in this process, as a person types it and
//! as a runner launches it.

use assembly_line::config::REPO_CONFIG_PATH;
use assembly_line::frame::Frame;
use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use std::path::{Path, PathBuf};

mod support;

/// An opted-in repository published to a bare origin, a root nothing should
/// write to, and a scratch directory for the clone.
struct Fixture {
    tmp: tempfile::TempDir,
    repo: PathBuf,
    origin: PathBuf,
}

impl Fixture {
    async fn running(script: &str) -> Self {
        Self::with_config(&format!("verify = \"true\"\n{}", support::config_running(script)))
            .await
    }

    async fn with_config(config: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        support::init_git_repo(&repo).await;
        std::fs::create_dir_all(repo.join(".assembly")).unwrap();
        std::fs::write(
            repo.join(REPO_CONFIG_PATH),
            format!("{config}\n[delivery]\nmode = \"none\"\n"),
        )
        .unwrap();
        support::commit_all(&repo, "opt in").await.unwrap().unwrap();
        let origin = tmp.path().join("origin.git");
        support::add_origin(&repo, &origin).await;
        support::publish_main(&repo).await;
        Fixture { tmp, repo, origin }
    }

    fn root(&self) -> PathBuf {
        self.tmp.path().join("root")
    }

    /// `assembly` in the repository, with a `gh` that refuses first on
    /// `PATH`, its root and scratch inside this fixture.
    fn assembly(&self) -> Command {
        let mut cmd = Command::cargo_bin("assembly").unwrap();
        cmd.current_dir(&self.repo)
            .env("PATH", support::path_where_gh_refuses())
            .env("ASSEMBLY_ROOT", self.root())
            .env("TMPDIR", self.tmp.path().join("scratch"));
        cmd
    }

    fn on_origin(&self, args: &[&str]) -> String {
        git_in(&self.origin, args)
    }
}

fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[tokio::test]
async fn run_does_a_whole_job_and_keeps_no_state() {
    let fx = Fixture::running("fake-agent.sh").await;

    fx.assembly()
        .args(["run", "--prompt", "do the thing"])
        .assert()
        .success()
        .stdout(contains("job 1: passed").and(contains("branch: al/job-1")));

    assert!(fx.on_origin(&["show", "al/job-1:agent-output.txt"]).contains("do the thing"));
    assert!(!fx.root().exists(), "run kept state under the root");
    assert_eq!(git_in(&fx.repo, &["status", "--porcelain"]), "");
}

#[tokio::test]
async fn run_takes_a_remote_url_and_then_needs_a_ref() {
    let fx = Fixture::running("fake-agent.sh").await;
    let url = fx.origin.to_str().unwrap();

    fx.assembly()
        .args(["run", "--repo", url, "--prompt", "x"])
        .assert()
        .code(2)
        .stderr(contains("--ref"));

    fx.assembly()
        .args(["run", "--repo", url, "--ref", "main", "--prompt", "x"])
        .assert()
        .success();
    assert_eq!(fx.on_origin(&["rev-parse", "--verify", "-q", "al/job-1"]).len(), 40);
}

#[tokio::test]
async fn with_frames_stdout_is_frames_and_nothing_else() {
    let fx = Fixture::running("fake-agent.sh").await;

    let out = fx
        .assembly()
        .args(["run", "--prompt", "x", "--frames"])
        .output()
        .unwrap();

    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    let frames: Vec<Frame> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{line}: {e}")))
        .collect();
    assert!(stdout.contains("\"t\":\"round_passed\""), "{stdout}");
    assert!(!stdout.contains("round_started"), "run numbered its own round: {stdout}");
    assert_eq!(frames.first().map(|f| f.seq), Some(1));
}

/// A revise's agent gets only what to change: its earlier work is the
/// branch, and what earlier rounds were asked is in the branch's history.
#[tokio::test]
async fn run_on_a_job_continues_its_branch_with_only_the_new_prompt() {
    let fx = Fixture::running("revising-agent.sh").await;
    fx.assembly().args(["run", "--prompt", "hi"]).assert().success();

    fx.assembly()
        .args(["run", "--job", "1", "--prompt", "add error handling"])
        .assert()
        .success()
        .stdout(contains("job 1: passed"));

    assert_eq!(fx.on_origin(&["rev-list", "--count", "al/job-1"]), "4");
    let rounds = fx.on_origin(&["show", "al/job-1:rounds.txt"]);
    assert_eq!(rounds, "hi\nadd error handling", "{rounds}");
}

#[tokio::test]
async fn run_on_a_job_with_no_branch_is_refused() {
    let fx = Fixture::running("fake-agent.sh").await;

    fx.assembly()
        .args(["run", "--job", "9", "--prompt", "x"])
        .assert()
        .code(2)
        .stderr(contains("job 9 has no branch"));
}

/// Refused before the claim: a broken config costs no id and leaves no
/// branch on the remote.
#[tokio::test]
async fn a_config_that_cannot_run_is_refused_before_anything_is_claimed() {
    let fx = Fixture::with_config("provider = \"ghost\"\nmax_duration = \"soon\"").await;

    fx.assembly()
        .args(["run", "--prompt", "x"])
        .assert()
        .code(2)
        .stderr(contains("'ghost'").and(contains("max_duration 'soon'")));

    assert_eq!(fx.on_origin(&["branch", "--list", "al/job-*"]), "");
}

#[tokio::test]
async fn a_pinned_base_is_used_even_after_the_ref_moved() {
    let fx = Fixture::running("fake-agent.sh").await;
    let pinned = git_in(&fx.repo, &["rev-parse", "HEAD"]);
    std::fs::write(fx.repo.join("later.txt"), "later\n").unwrap();
    support::commit_all(&fx.repo, "later").await.unwrap().unwrap();
    support::publish_main(&fx.repo).await;

    fx.assembly()
        .args(["run", "--ref", &format!("main@{pinned}"), "--prompt", "x"])
        .assert()
        .success();

    assert_eq!(fx.on_origin(&["rev-parse", "al/job-1^"]), pinned);
}

#[tokio::test]
async fn the_agent_never_sees_the_forge_token() {
    let fx = Fixture::running("forge-token-reporting-agent.sh").await;

    fx.assembly()
        .env("GH_TOKEN", "s3cret")
        .args(["run", "--prompt", "x"])
        .assert()
        .success();

    assert_eq!(fx.on_origin(&["show", "al/job-1:forge-token.txt"]), "absent");
}
```

`tests/delivery.rs` — the file's `fake_gh_capturing_args` succeeds for every
subcommand, which would now make every delivery look like an already-open
pull request. Make its `pr view` answer only when `$FAKE_GH_EXISTING` is set:

```rust
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho \"$@\" >> {}\n\
             if [ \"$1 $2\" = \"pr view\" ]; then\n\
               [ -n \"${{FAKE_GH_EXISTING:-}}\" ] || exit 1\n\
               echo \"$FAKE_GH_EXISTING\"; exit 0\n\
             fi\n\
             echo https://example.invalid/pr/1\n",
            capture.display()
        ),
    )
```

Existing assertions on the captured invocation use `contains`, so the extra
`pr view` line does not disturb them. Then two tests of `run`'s own
delivery:

```rust
#[tokio::test]
async fn run_opens_the_pull_request_from_inside_its_clone() {
    let tmp = repo_running("fake-agent.sh", "true").await;
    let (gh_dir, args_log) = fake_gh_capturing_args(&tmp);

    assembly(&tmp)
        .env("PATH", path_with(&gh_dir))
        .args(["run", "--prompt", "add auth"])
        .assert()
        .success()
        .stdout(contains("pull request: https://example.invalid/pr/1"));

    let args = std::fs::read_to_string(&args_log).unwrap();
    assert!(args.contains("pr create --base main --head al/job-1"), "{args}");
}

/// A revise's commits land on the pull request its job already has, so
/// asking for another would only fail.
#[tokio::test]
async fn a_revise_reports_the_pull_request_it_already_has() {
    let tmp = repo_running("fake-agent.sh", "true").await;
    let (gh_dir, args_log) = fake_gh_capturing_args(&tmp);
    assembly(&tmp)
        .env("PATH", path_with(&gh_dir))
        .args(["run", "--prompt", "a"])
        .assert()
        .success();
    std::fs::write(&args_log, "").unwrap();

    assembly(&tmp)
        .env("PATH", path_with(&gh_dir))
        .env("FAKE_GH_EXISTING", "https://example.invalid/pr/1")
        .args(["run", "--job", "1", "--prompt", "b"])
        .assert()
        .success()
        .stdout(contains("pull request: https://example.invalid/pr/1"));

    assert!(!std::fs::read_to_string(&args_log).unwrap().contains("pr create"));
}
```

`path_with(dir)` puts `dir` first on `PATH`; add it beside
`fake_gh_capturing_args` if the file has no such helper.

- [ ] **Step 6: Run them to make sure they fail**

Run: `cargo test --test run`
Expected: FAIL — `unrecognized subcommand 'run'`.

- [ ] **Step 7: Write `src/run.rs`**

```rust
//! `assembly run`: one whole job, in this process — what every runner
//! launches, and what a person types to reproduce a job.
//!
//! It keeps no state. What it did is its output — frames for a collector, or
//! readable lines for a person — and the branch and pull request it leaves.
//! Preparing claims nothing until every check that can refuse the job has
//! passed, so a refusal costs no id.

use crate::claim::claim_job;
use crate::config::{self, ConfigError, RepoConfig};
use crate::delivery::{self, Delivered, PullRequestText};
use crate::event::EventKind;
use crate::frame::FrameWriter;
use crate::git::{self, PinnedRef};
use crate::job::JobId;
use crate::payload::{GIT_TOKEN_VAR, RoundPayload, RoundRequest, remote_to_clone};
use crate::report::JobReport;
use crate::round::{PROVISIONING_LIMIT, Verdict, run_round_in};
use crate::workspace::{self, DEFAULT_REMOTE, ScratchClone};
use anyhow::anyhow;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tokio_util::sync::CancellationToken;

/// What `assembly run` was asked for, as given on the command line.
#[derive(Debug, Clone, Default)]
pub struct RunRequest {
    /// A checkout, or a remote URL. `None` is the enclosing checkout.
    pub repo: Option<String>,
    /// `REF`, or `REF@SHA` to pin it.
    pub base_ref: Option<String>,
    pub job: Option<u64>,
    pub prompt: Option<String>,
    pub prompt_file: Option<PathBuf>,
    pub provider: Option<String>,
    pub provision_toolchain: bool,
}

/// Why a job will not run, decided before anything was claimed for it.
#[derive(Debug)]
pub enum RunRefused {
    ConfigNotRunnable(Vec<ConfigError>),
    NoJobBranch(JobId),
    Unpreparable(anyhow::Error),
}

impl RunRefused {
    /// Every reason, one per line, to report ahead of the refusal itself.
    #[must_use]
    pub fn itemized_reasons(&self) -> Vec<String> {
        match self {
            Self::ConfigNotRunnable(errors) => errors.iter().map(ToString::to_string).collect(),
            Self::NoJobBranch(_) | Self::Unpreparable(_) => Vec::new(),
        }
    }
}

impl std::fmt::Display for RunRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConfigNotRunnable(_) => write!(f, "{} is not runnable", config::REPO_CONFIG_PATH),
            Self::NoJobBranch(id) => write!(
                f,
                "job {id} has no branch on the remote — there is nothing to continue; run \
                 without --job to start a new job"
            ),
            Self::Unpreparable(e) => write!(f, "{e}"),
        }
    }
}

/// The pull request targets a different branch from the one the job was cut
/// from, so its diff carries more than the job's work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseDiffers {
    pub base: String,
    pub base_ref: String,
}

impl std::fmt::Display for BaseDiffers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "this pull request will target '{}', but the job was cut from '{}' — review the \
             diff before merging, since it carries everything separating the two, not just this \
             job's work",
            self.base, self.base_ref
        )
    }
}

/// A job that has passed every check: cloned, its config read and valid, its
/// id claimed or its branch found.
#[derive(Debug)]
pub struct ReadyJob {
    clone: ScratchClone,
    payload: RoundPayload,
    config: RepoConfig,
    base: PinnedRef,
    prompt: String,
    claimed: bool,
    provisioning_deadline: Instant,
}

/// What a finished job left.
#[derive(Debug)]
pub struct JobConclusion {
    pub job: JobId,
    pub verdict: Verdict,
    pub report: JobReport,
    pub base_differs: Option<BaseDiffers>,
}

impl JobConclusion {
    /// What the job left, as printed once it is over.
    #[must_use]
    pub fn to_lines(&self) -> Vec<String> {
        self.base_differs
            .iter()
            .map(|differs| format!("note: {differs}"))
            .chain(self.report.to_status_lines())
            .collect()
    }
}

/// Everything `run` checks, and the claim, before the round.
///
/// # Errors
///
/// A [`RunRefused`] for every reason the job cannot run; nothing is claimed
/// on the remote for a refused job.
pub async fn prepare_run(
    request: RunRequest,
    scratch_root: &Path,
    cancel: &CancellationToken,
) -> Result<ReadyJob, RunRefused> {
    let prompt = prompt_text(request.prompt, request.prompt_file).map_err(RunRefused::Unpreparable)?;
    let (remote_url, default_ref) = remote_and_default_ref(request.repo)
        .await
        .map_err(RunRefused::Unpreparable)?;
    let (ref_name, pinned_sha) = match (request.base_ref, default_ref) {
        (Some(named), _) => split_pin(&named),
        (None, Some(checked_out)) => (checked_out, None),
        (None, None) => {
            return Err(RunRefused::Unpreparable(anyhow!(
                "a remote URL has no checked-out branch — name the ref to start from with --ref"
            )));
        }
    };

    // Set before the clone, so a slow clone leaves less of the shared budget
    // for provisioning rather than a fresh one of its own.
    let provisioning_deadline = Instant::now() + PROVISIONING_LIMIT;
    let helper = std::env::var_os(GIT_TOKEN_VAR)
        .is_some()
        .then_some(git::TOKEN_CREDENTIAL_HELPER);
    let cloning = tokio::time::timeout(
        PROVISIONING_LIMIT,
        workspace::clone_scratch(&remote_url, scratch_root, helper),
    );
    let clone = tokio::select! {
        cloned = cloning => cloned
            .map_err(|_| anyhow!("cloning timed out after {}", humantime::format_duration(PROVISIONING_LIMIT)))
            .and_then(|cloned| cloned)
            .map_err(RunRefused::Unpreparable)?,
        () = cancel.cancelled() => return Err(RunRefused::Unpreparable(anyhow!("cancelled"))),
    };

    let base = workspace::pin_in_clone(&clone, &ref_name, pinned_sha.as_deref())
        .await
        .map_err(RunRefused::Unpreparable)?;
    let config = RepoConfig::from_ref(clone.path(), &base.sha)
        .await
        .map_err(RunRefused::Unpreparable)?;
    let provider = request
        .provider
        .or_else(|| config.provider.clone())
        .unwrap_or_default();
    let problems = config.reasons_it_cannot_run(&provider);
    if !problems.is_empty() {
        return Err(RunRefused::ConfigNotRunnable(problems));
    }

    let (job_id, start, claimed) = match request.job {
        Some(id) => {
            let id = JobId::from(id);
            let tip = workspace::job_branch_in_clone(&clone, &id.branch_name())
                .await
                .map_err(RunRefused::Unpreparable)?
                .ok_or(RunRefused::NoJobBranch(id))?;
            (id, tip, false)
        }
        None => {
            let id = claim_job(clone.path(), DEFAULT_REMOTE, &base.sha)
                .await
                .map_err(RunRefused::Unpreparable)?;
            (id, base.clone(), true)
        }
    };

    let payload = RoundPayload {
        provision_toolchain: request.provision_toolchain,
        ..RoundPayload::for_round(
            &config,
            RoundRequest {
                job_id,
                // `run` numbers no rounds; the field goes with `job-exec`.
                round: 0,
                prompt: &prompt,
                provider: &provider,
                start,
                remote_name: DEFAULT_REMOTE,
                remote_url,
            },
        )
        .map_err(RunRefused::Unpreparable)?
    };

    Ok(ReadyJob {
        clone,
        payload,
        config,
        base,
        prompt,
        claimed,
        provisioning_deadline,
    })
}

impl ReadyJob {
    /// Which job is about to run, and from where.
    #[must_use]
    pub fn to_announcement_line(&self) -> String {
        match self.claimed {
            true => format!(
                "job {}: claimed {} at {} ({})",
                self.payload.job_id,
                self.payload.branch,
                self.base.name,
                &self.base.sha[..12.min(self.base.sha.len())]
            ),
            false => format!("job {}: continuing {}", self.payload.job_id, self.payload.branch),
        }
    }

    /// Run the round, deliver its branch if it passed, and conclude.
    ///
    /// # Errors
    ///
    /// Only when the round's frames cannot be written.
    pub async fn run<W: Write + Send + 'static>(
        self,
        frames: &FrameWriter<W>,
        cancel: CancellationToken,
    ) -> anyhow::Result<JobConclusion> {
        let ReadyJob {
            clone,
            payload,
            config,
            base,
            prompt,
            provisioning_deadline,
            ..
        } = self;
        let ws = match workspace::start_round(clone, &payload.start, &payload.branch).await {
            Ok(ws) => ws,
            Err(e) => {
                frames.append_event(EventKind::RoundFailed { reason: e.to_string() })?;
                return conclude(&payload, frames, Verdict::Failed, None);
            }
        };
        let verdict = run_round_in(&payload, &ws, frames, provisioning_deadline, cancel).await?;

        let pushed_this_round = frames
            .events_so_far()
            .iter()
            .any(|e| matches!(e.kind, EventKind::BranchPushed { .. }));
        let base_differs = match (verdict, pushed_this_round) {
            (Verdict::Passed, true) => {
                deliver(&ws, &config, &base, &payload, &prompt, frames).await?
            }
            _ => None,
        };
        if let Err(e) = workspace::discard(ws) {
            frames.append_output(&format!("could not remove the scratch checkout: {e}"))?;
        }
        conclude(&payload, frames, verdict, base_differs)
    }
}

/// Open the pull request from inside the clone, and record what came of it:
/// an event for a pull request that exists, a line of output otherwise.
async fn deliver<W: Write + Send + 'static>(
    ws: &workspace::RoundWorkspace,
    config: &RepoConfig,
    base: &PinnedRef,
    payload: &RoundPayload,
    prompt: &str,
    frames: &FrameWriter<W>,
) -> anyhow::Result<Option<BaseDiffers>> {
    let target = config.base.as_deref().unwrap_or(&base.name);
    let base_differs = (target != base.name).then(|| BaseDiffers {
        base: target.to_string(),
        base_ref: base.name.clone(),
    });
    let title = payload.commit_message.lines().next().unwrap_or_default();
    let delivered = delivery::deliver(
        ws.path(),
        &config.delivery,
        &payload.branch,
        target,
        PullRequestText { title, body: prompt },
    )
    .await;
    match delivered {
        Delivered::Opened { url } | Delivered::AlreadyOpen { url } => {
            frames.append_event(EventKind::PullRequestOpened { url })?;
        }
        other => frames.append_output(&other.to_string())?,
    }
    Ok(base_differs)
}

fn conclude<W: Write>(
    payload: &RoundPayload,
    frames: &FrameWriter<W>,
    verdict: Verdict,
    base_differs: Option<BaseDiffers>,
) -> anyhow::Result<JobConclusion> {
    let events = frames.events_so_far();
    Ok(JobConclusion {
        job: payload.job_id,
        verdict,
        report: JobReport::from_events(payload.job_id.into(), &events),
        base_differs,
    })
}

/// The prompt, from the command line or the file it names.
///
/// # Errors
///
/// When the file cannot be read.
pub fn prompt_text(prompt: Option<String>, prompt_file: Option<PathBuf>) -> anyhow::Result<String> {
    match (prompt, prompt_file) {
        (Some(text), _) => Ok(text),
        (None, Some(path)) => {
            std::fs::read_to_string(&path).map_err(|e| anyhow!("reading {}: {e}", path.display()))
        }
        // clap refuses this combination before we are reached.
        (None, None) => Err(anyhow!("a job needs --prompt or --prompt-file")),
    }
}

/// Where the job clones from, and the branch to start from when `--ref`
/// names none: a checkout's checked-out branch, and nothing for a URL.
async fn remote_and_default_ref(repo: Option<String>) -> anyhow::Result<(String, Option<String>)> {
    let checkout = match repo {
        Some(named) if Path::new(&named).join(".git").exists() => PathBuf::from(named),
        Some(url) => return Ok((url, None)),
        None => {
            let cwd = std::env::current_dir()?;
            crate::paths::git_root(&cwd).ok_or_else(|| {
                anyhow!("not inside a git repository — name one with --repo")
            })?
        }
    };
    let remote_url = remote_to_clone(&checkout, DEFAULT_REMOTE).await?;
    let branch = git::current_branch(&checkout).await?;
    match branch {
        Some(branch) => Ok((remote_url, Some(branch))),
        None => Err(anyhow!("HEAD is detached — name the ref to start from with --ref")),
    }
}

/// `main@<sha>` as the ref and the commit it is pinned to; anything whose
/// part after the last `@` is not a full hex object name is a ref alone.
fn split_pin(named: &str) -> (String, Option<String>) {
    match named.rsplit_once('@') {
        Some((name, sha))
            if !name.is_empty()
                && matches!(sha.len(), 40 | 64)
                && sha.chars().all(|c| c.is_ascii_hexdigit()) =>
        {
            (name.to_string(), Some(sha.to_string()))
        }
        _ => (named.to_string(), None),
    }
}
```

`RoundPayload::for_round` sets `provision_toolchain: false`, so `run` sets it
from the request with struct-update syntax as above. `RoundPayload` needs
`#[derive(Debug)]` (it has it).

`src/lifecycle.rs`: delete its `prompt_text` and `BaseDiffers` and import
both from `crate::run`.

- [ ] **Step 8: Wire the command**

`src/cli.rs`, a new first variant:

```rust
    /// Do one whole job, here, in this process: clone, agent, verify, push,
    /// pull request. What every runner launches.
    Run {
        /// What the agent is asked to do
        #[arg(
            long,
            conflicts_with = "prompt_file",
            required_unless_present = "prompt_file"
        )]
        prompt: Option<String>,
        /// Read the prompt from a file instead
        #[arg(long)]
        prompt_file: Option<PathBuf>,
        /// A checkout, or a remote URL. Defaults to the enclosing checkout.
        #[arg(long)]
        repo: Option<String>,
        /// What to start from: a ref, or REF@SHA to pin it. Defaults to the
        /// checkout's branch.
        #[arg(long = "ref")]
        base_ref: Option<String>,
        /// Work on this job's branch instead of claiming a new job
        #[arg(long)]
        job: Option<u64>,
        /// Overrides the repository's declared provider
        #[arg(long)]
        provider: Option<String>,
        /// Report as NDJSON frames on stdout, for a collector
        #[arg(long)]
        frames: bool,
        /// Install the repository's toolchain with mise first. Set by
        /// container runners.
        #[arg(long, hide = true)]
        provision_toolchain: bool,
    },
```

`src/main.rs` — the arm builds a `RunRequest` from the fields and calls:

```rust
/// `run`: prepare, announce, run, conclude. Frames go to stdout and
/// everything a person reads to stderr; without frames, a person reads
/// stdout.
async fn run_whole_job(request: RunRequest, as_frames: bool) -> Result<ExitCode, String> {
    let cancel = CancellationToken::new();
    cancel_on_termination_signal(cancel.clone());
    let ready = run::prepare_run(request, &std::env::temp_dir(), &cancel)
        .await
        .map_err(|refused| report_run_refusal(&refused))?;

    let conclusion = match as_frames {
        true => {
            eprintln!("{}", ready.to_announcement_line());
            ready.run(&FrameWriter::new(std::io::stdout()), cancel).await
        }
        false => {
            println!("{}", ready.to_announcement_line());
            ready
                .run(&FrameWriter::new(ReadableFrames::new(std::io::stdout())), cancel)
                .await
        }
    }
    .map_err(|e| e.to_string())?;

    let lines = conclusion.to_lines();
    match as_frames {
        true => lines.iter().for_each(|line| eprintln!("{line}")),
        false => lines.iter().for_each(|line| println!("{line}")),
    }
    Ok(exit_code_for(conclusion.verdict))
}

fn report_run_refusal(refused: &RunRefused) -> String {
    refused
        .itemized_reasons()
        .iter()
        .for_each(|reason| eprintln!("error: {reason}"));
    refused.to_string()
}
```

- [ ] **Step 9: Run the gate**

Run: `just check`
Expected: PASS, with `tests/run.rs`'s tests and the unit tests above added.

- [ ] **Step 10: Commit**

```bash
jj describe -m "feat(run): assembly run does one whole job in this process

run resolves the repository and base, clones into scratch, reads and
validates the config at the base's commit, claims an id or continues
--job's branch, runs the round, pushes and opens the pull request from
inside its clone. It keeps no state; --frames makes its stdout NDJSON
for a collector. GH_TOKEN is withheld from the agent. submit's runners
still launch job-exec until the next change.

Tests: <count>."
jj new
```

---

### Task 7: Runners launch `assembly run`; `job-exec` and `ASSEMBLY_JOB` go

Every runner now launches `assembly run --repo=… --ref=<base>@<sha>
--job=N --prompt=… --provider=… --frames`, plus `--provision-toolchain` in a
container. The host (`submit`, until Task 10) claims the job's id before
launching, so the round always runs with `--job`: a new job and a revise are
the same launch. The host no longer builds a payload, reads no config beyond
validating it, and no longer delivers — `run` does, from inside the
boundary. It writes `RoundRequested` and `RoundStarted` itself before the
launch (P1), so the collector never has to invent a start.

Every argument is `--flag=value` (P2): a prompt that begins with `-` is a
value, not a flag.

`RoundPayload` stays as the in-process plan `run` builds for `run_round_in`,
without serde, `round`, or the environment variable it used to travel in.
The revise prompt is the feedback alone (Decision 6): `revised_prompt` goes.

**Files:**
- Modify: `src/runner/mod.rs`, `src/runner/local.rs`, `src/runner/docker.rs`, `src/runner/kubernetes.rs`, `src/lifecycle.rs`, `src/collect.rs`, `src/round.rs`, `src/payload.rs`, `src/exec.rs`, `src/config.rs`, `src/cli.rs`, `src/main.rs`, `src/run.rs`, `Dockerfile`, `scripts/smoke-docker.sh`, `justfile` (`smoke-docker` comment)
- Test: `tests/runner.rs`, `tests/collect.rs`, `tests/docker_runner.rs`, `tests/kubernetes_runner.rs`, `tests/round.rs`, `tests/provisioning.rs`, `tests/payload.rs`, `tests/lifecycle.rs`, `tests/cli.rs`, `tests/delivery.rs`, `tests/support/mod.rs`, `tests/fixtures/env-reporting-agent.sh`

**Interfaces:**
- Consumes: `claim::claim_job`, `payload::https_equivalent`, `payload::FORGE_TOKEN_VAR`.
- Produces:
  - `runner::LaunchSpec { name: String, args: Vec<String>, command_limit_secs: Option<u64> }` and
    `LaunchSpec::for_round<R: Runner>(job: JobId, round: u32, remote_url: &str, base: &PinnedRef, prompt: &str, provider: &str, command_limit_secs: Option<u64>) -> LaunchSpec`
  - `Runner::launch(&self, spec: &LaunchSpec, secrets: &JobSecrets, cancel: &CancellationToken)`
  - `runner::kubernetes::job_manifest(name: &str, image: &str, args: &[String], active_deadline_secs: Option<u64>) -> Value`; `active_deadline_secs(command_limit_secs: Option<u64>) -> Option<u64>`
  - `runner::docker::docker_run_args(image: &str, container: &str, env_names: &[&str], args: &[String]) -> Vec<String>`
  - `collect::collect(running, log, output_log, cancel)` (no `round`); `collect::record_launch_failure(log, error)` (no `round`)
  - `config::RepoConfig::command_limit_secs(&self) -> Option<u64>`
- Deleted: `payload::PAYLOAD_VAR`, `RoundPayload::{from_environment, from_variable, round}`, `RoundRequest::round`, `payload::revised_prompt`, `runner::payload_fitted_to`, `round::run_round`, `cli::Command::JobExec`, `lifecycle::Handoff`, delivery from `lifecycle`.

- [ ] **Step 1: Write the failing tests**

`tests/runner.rs` — replace `payload_cloning` and the two tests that use it,
and `docker_run_names_its_secrets_and_runs_job_exec`:

```rust
fn base() -> PinnedRef {
    PinnedRef { name: "main".into(), sha: "a".repeat(40) }
}

#[test]
fn a_container_round_runs_assembly_run_over_https_and_provisions_first() {
    let spec = LaunchSpec::for_round::<ContainerRunner>(
        JobId::from(7), 2, "git@github.com:o/r.git", &base(), "fix it", "claude", Some(60),
    );

    assert_eq!(
        spec.args,
        [
            "run".to_string(),
            "--repo=https://github.com/o/r.git".into(),
            format!("--ref=main@{}", "a".repeat(40)),
            "--job=7".into(),
            "--prompt=fix it".into(),
            "--provider=claude".into(),
            "--frames".into(),
            "--provision-toolchain".into(),
        ]
    );
    assert_eq!(spec.command_limit_secs, Some(60));
    assert!(spec.name.starts_with("al-7-2-"), "{}", spec.name);
}

#[test]
fn a_host_round_runs_assembly_run_with_the_remote_and_toolchain_as_they_are() {
    let spec = LaunchSpec::for_round::<HostRunner>(
        JobId::from(7), 1, "git@github.com:o/r.git", &base(), "x", "claude", None,
    );

    assert!(spec.args.contains(&"--repo=git@github.com:o/r.git".to_string()));
    assert!(!spec.args.contains(&"--provision-toolchain".to_string()));
}

/// Review focus 1, at the seam: every value rides after `=`, so no prompt
/// can be mistaken for a flag.
#[test]
fn every_value_on_the_command_line_is_attached_to_its_flag() {
    let spec = LaunchSpec::for_round::<HostRunner>(
        JobId::from(1), 1, "/o.git", &base(), "--frames\n\"quoted\"", "p", None,
    );

    assert!(spec.args.contains(&"--prompt=--frames\n\"quoted\"".to_string()));
}

#[test]
fn docker_run_names_its_secrets_and_runs_assembly_run() {
    let args = docker_run_args(
        "img:1",
        "al-1-1-x",
        &["ASSEMBLY_GIT_TOKEN", "GH_TOKEN"],
        &["run".into(), "--job=1".into()],
    );
    let joined = args.join(" ");
    assert!(joined.starts_with("run --name al-1-1-x -e ASSEMBLY_GIT_TOKEN -e GH_TOKEN"), "{joined}");
    assert!(joined.ends_with("img:1 assembly run --job=1"), "{joined}");
}

#[test]
fn a_container_always_receives_both_tokens() {
    let (secrets, problems) = JobSecrets::from_lookup(&[], |name| Some(format!("{name}-value")));

    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(
        secrets.names().into_iter().collect::<Vec<_>>(),
        [FORGE_TOKEN_VAR, GIT_TOKEN_VAR]
    );
}
```

Update `a_container_always_receives_the_git_token_and_only_the_named_extras`,
`a_container_runner_carries_the_git_token_from_the_host` and
`every_missing_variable_is_reported_at_once` for `GH_TOKEN` being sent
always: a host with only the git token now reports
`RunnerProblem::MissingEnvironment("GH_TOKEN".into())`, so make
`host_with_only_the_git_token` answer for both tokens and rename it
`host_with_both_tokens`. In `passing_a_variable_assembly_line_sets_itself_is_refused`,
`--pass-env ASSEMBLY_JOB` is no longer reserved (nothing sets it); use
`GH_TOKEN`.

`tests/round.rs`, through the harness (host → local runner → `assembly run`):

```rust
/// Review focus 1, end to end: a prompt shaped like a flag, with quotes and
/// a newline in it, reaches the agent exactly.
#[tokio::test]
async fn a_prompt_that_looks_like_a_flag_reaches_the_agent_intact() {
    let h = Harness::new().await;
    let prompt = "--frames --job=9\nsay \"hi\" and 'bye'";

    let outcome = h.run_job(prompt).await;

    assert!(outcome.passed, "{:?}", outcome.events);
    assert_eq!(
        h.file_on_remote_branch(&outcome.job_id.branch_name(), "agent-output.txt")
            .await
            .as_deref(),
        Some(format!("{prompt}\n").as_str())
    );
}
```

(`fake-agent.sh` writes `printf '%s\n' "$1"` to `agent-output.txt`, so the
file is the prompt and one newline, byte for byte.)

- [ ] **Step 2: Run them to make sure they fail**

Run: `cargo test --test runner`
Expected: FAIL to compile — `no LaunchSpec in runner`.

- [ ] **Step 3: The launch spec and the runners**

`src/runner/mod.rs` — delete `payload_fitted_to`, and replace
`job_resource_name`:

```rust
/// What a runner launches: `assembly` with `args`, for one round of one job.
/// The arguments are the round's whole instruction — the command line a
/// person types to reproduce it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    /// Unique to this round, for the container or Job that runs it, so two
    /// repositories' job 1 never collide.
    pub name: String,
    /// `assembly`'s arguments, `run` first.
    pub args: Vec<String>,
    /// The repository's `max_duration`, for a runner that enforces a
    /// backstop of its own behind `run`'s.
    pub command_limit_secs: Option<u64>,
}

impl LaunchSpec {
    /// Round `round` of job `job`, fitted to where `R` runs it: a container
    /// has neither the host's toolchain nor its SSH keys, so it provisions
    /// the one and reaches the remote over HTTPS, with a token, in place of
    /// the other. Every value is attached with `=`, so none can be read as
    /// a flag.
    #[must_use]
    pub fn for_round<R: Runner>(
        job: JobId,
        round: u32,
        remote_url: &str,
        base: &PinnedRef,
        prompt: &str,
        provider: &str,
        command_limit_secs: Option<u64>,
    ) -> LaunchSpec {
        let remote_url = match R::RUNS_IN_A_CONTAINER {
            true => https_equivalent(remote_url),
            false => remote_url.to_string(),
        };
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        LaunchSpec {
            name: format!("al-{job}-{round}-{nanos:x}"),
            args: [
                "run".to_string(),
                format!("--repo={remote_url}"),
                format!("--ref={}@{}", base.name, base.sha),
                format!("--job={job}"),
                format!("--prompt={prompt}"),
                format!("--provider={provider}"),
                "--frames".to_string(),
            ]
            .into_iter()
            .chain(R::RUNS_IN_A_CONTAINER.then(|| "--provision-toolchain".to_string()))
            .collect(),
            command_limit_secs,
        }
    }
}
```

`Runner::launch` takes `spec: &LaunchSpec` in place of `payload: &RoundPayload`.
`RESERVED_ENVIRONMENT` becomes `[GIT_TOKEN_VAR, FORGE_TOKEN_VAR]`, and
`JobSecrets::from_lookup` looks up both always:

```rust
        let names: Vec<&str> = RESERVED_ENVIRONMENT.into_iter().chain(chosen).collect();
```

Its doc: "Both tokens, always, plus each name in `pass_env`…". The
`--pass-env` help in `cli.rs` says "`ASSEMBLY_GIT_TOKEN` and `GH_TOKEN` are
always passed." `published_image`'s doc: "For a release, the `run` inside is
built from the same tag, so the collector and the round agree about the frame
format; …".

`src/runner/local.rs`:

```rust
impl LocalRunner {
    fn spawn_run(&self, spec: &LaunchSpec) -> anyhow::Result<LocalRound> {
        let mut command = Command::new(&self.program);
        command
            .args(&spec.args)
            // Exported for a container runner, the token would make `run`
            // authenticate with it instead of the host's own credentials.
            .env_remove(GIT_TOKEN_VAR);
        ChildLines::spawn(command).map(|lines| LocalRound { lines })
    }
}
```

`LocalRunner::current_binary`'s doc: "This very binary, which is what
`assembly run` is." `LocalRound::cancel`'s doc: "SIGTERM, which `run` answers
by cancelling its agent and reporting the round."

`src/runner/docker.rs`:

```rust
#[must_use]
pub fn docker_run_args(
    image: &str,
    container: &str,
    env_names: &[&str],
    args: &[String],
) -> Vec<String> {
    ["run", "--name", container]
        .into_iter()
        .map(String::from)
        .chain(env_names.iter().flat_map(|name| ["-e".to_string(), (*name).to_string()]))
        .chain([image.to_string(), "assembly".to_string()])
        .chain(args.iter().cloned())
        .collect()
}
```

`spawn_docker_run(spec, secrets)` names only the secrets
(`secrets.names()`) and sets their values with `.envs(secrets.vars())`; no
payload. The container is `spec.name`.

`src/runner/kubernetes.rs`: `active_deadline_secs(command_limit_secs:
Option<u64>)`; `job_manifest(name, image, args, active_deadline_secs)` with
`"command": std::iter::once("assembly").chain(args.iter().map(String::as_str)).collect::<Vec<_>>()`;
the Secret carries `secrets.vars()` only. `launch` names the round
`spec.name`.

- [ ] **Step 4: The host claims, records and launches; the collector only collects**

`src/config.rs`:

```rust
    /// `max_duration` in whole seconds, when it is set and parses.
    #[must_use]
    pub fn command_limit_secs(&self) -> Option<u64> {
        self.max_duration
            .as_deref()
            .and_then(|value| parse_duration(value).ok())
            .map(|limit| limit.as_secs())
    }
```

`src/collect.rs`: `collect` loses `round` and `start_missing_from`;
`record_launch_failure(log, error)` appends only the `RoundFailed`. Update
the module doc's "`job-exec`" mentions to "`run`".

`src/lifecycle.rs`:

- `ReadyRound` loses `start`'s role as the payload start; it keeps
  `requested_base: PinnedRef` (what the round's `--ref` pins) and
  `remote_url`, `round_prompt`, `provider`, `config`, `secrets`.
- `prepare_revision`'s `round_prompt` is the prompt text as given (delete
  the `revised_prompt` call).
- `run`:

```rust
pub async fn run<R: Runner>(
    ready: ReadyRound<'_, R>,
    cancel: CancellationToken,
) -> anyhow::Result<RoundConclusion> {
    let ReadyRound {
        runner,
        repo,
        destination,
        requested_base,
        remote_url,
        round_prompt,
        provider,
        config,
        secrets,
        ..
    } = ready;
    let started_the_job = matches!(destination, Destination::NewJob { .. });
    let (paths, mut log, round) = match destination {
        Destination::NewJob { jobs_dir } => {
            allocate_job(&jobs_dir, &repo, &requested_base.sha)
                .await
                .map(|(paths, log)| (paths, log, 1))?
        }
        Destination::ExistingJob { paths, round, log } => (paths, log, round),
    };

    log.append(EventKind::RoundRequested {
        remote_url: remote_url.clone(),
        base: requested_base.clone(),
        prompt: round_prompt.clone(),
        provider: provider.clone(),
    })?;
    log.append(EventKind::RoundStarted { round })?;
    let spec = LaunchSpec::for_round::<R>(
        paths.id,
        round,
        &remote_url,
        &requested_base,
        &round_prompt,
        &provider,
        config.command_limit_secs(),
    );
    let verdict = match runner.launch(&spec, &secrets, &cancel).await {
        Ok(running) => collect(running, &mut log, &paths.log(), cancel).await?,
        Err(e) => record_launch_failure(&mut log, &e)?,
    };

    let report = events_of(&paths)
        .ok()
        .map(|events| JobReport::from_events(paths.id.into(), &events));
    Ok(RoundConclusion {
        job: paths,
        verdict,
        report,
        started_the_job,
    })
}
```

- `RoundConclusion` loses `handoff`; `to_lines` is the report's
  `to_status_lines()` then the `state:` line. Delete `Handoff`, `hand_off`,
  `collect_round`, and the `delivery`, `BaseDiffers` and `payload` imports
  that only they used. `allocate_job` no longer writes anything but the
  directory and the open log (Task 4).
- The `revising job N (round R)` announcement is unchanged.

- [ ] **Step 5: Delete `job-exec` and the payload's travel**

- `src/cli.rs`: delete `Command::JobExec`.
- `src/main.rs`: delete its arm and `execute_payload_from_environment`;
  keep `cancel_on_termination_signal` (Task 6's `run_whole_job` uses it).
- `src/payload.rs`: delete `PAYLOAD_VAR`, `from_environment`,
  `from_variable`, `revised_prompt`, the `Serialize`/`Deserialize` derives
  and imports on `RoundPayload`, and `round` from `RoundPayload` and
  `RoundRequest`. The module doc becomes:

```rust
//! What one round runs, resolved by `run` in the process that runs it: the
//! repository's config applied to the job, its branch and its prompt.
```

  and `GIT_TOKEN_VAR`'s doc says "`run`'s own process environment" where it
  said "`job-exec`'s".
- `src/exec.rs`: drop `.env_remove(PAYLOAD_VAR)` and its import; the comment
  above the removes names only the two tokens.
- `src/round.rs`: delete `run_round` (the `job-exec` wrapper) and the
  `round` field from the unit test's payload. The module doc's "Running one
  round of a job" stays; `run_round_in` is the entry point.
- `src/run.rs`: drop `round: 0` from the `RoundRequest` literal.
- `tests/fixtures/env-reporting-agent.sh`: report
  `token=${ASSEMBLY_GIT_TOKEN:-absent} forge=${GH_TOKEN:-absent}`; update
  the test that reads its output (grep for `payload=`) to expect
  `forge=absent` instead of `payload=absent`.

- [ ] **Step 6: Move the tests that drove `job-exec` onto `run`**

- `tests/support/mod.rs`: replace `payload_for` with a launch spec for a job
  it claims first, so `run --job` finds the branch:

```rust
    /// A claimed job and the launch spec for its first round, for tests
    /// that hand a round to a runner directly.
    pub async fn launch_spec_for(&self, prompt: &str) -> LaunchSpec {
        let base = git::pinned(&self.repo, "origin", "main").await.unwrap();
        let job = claim::claim_job(&self.repo, "origin", &base.sha).await.unwrap();
        LaunchSpec::for_round::<LocalRunner>(
            job, 1, self.origin.to_str().unwrap(), &base, prompt, "fake", None,
        )
    }
```

  and `run_frames(&self, prompt, extra_env: &[(&str, &str)]) -> std::process::Output`,
  which runs `assembly` with `launch_spec_for(prompt).args`, `TMPDIR` set to
  `scratch_root()`, and `extra_env`, for the tests below that drove
  `job-exec` by hand.
- `tests/collect.rs`: every `h.payload_for(..)` becomes
  `h.launch_spec_for(..)`; `collect(.., 1, cancel)` loses the `1`;
  `record_launch_failure(&mut log, 1, &e)` loses the `1`. Rename
  `a_round_run_by_job_exec_is_collected_into_the_same_log_as_before` to
  `a_round_run_by_assembly_run_is_collected_into_the_same_log_as_before`
  and `a_job_exec_that_dies_without_a_verdict_is_recorded_as_failed` to
  `a_round_that_dies_without_a_verdict_is_recorded_as_failed`. Delete
  `a_round_that_dies_before_announcing_itself_is_still_recorded_as_that_round`
  — the host writes the start now, so there is nothing left for the
  collector to fill in. `a_runner_that_could_not_start_the_round_leaves_a_failed_round`
  asserts only the `RoundFailed`.
- `tests/round.rs`: `a_verify_that_cannot_start_still_records_the_pushed_branch`
  builds its `Harness::with_config` with the agent run by `/bin/bash`
  (`[providers.fake]\ncmd = "/bin/bash"`, same args) and `verify = "true"`,
  then `h.run_frames("x", &[("PATH", only_git)])` in place of the
  hand-built `job-exec`. `a_hangup_cancels_the_round_rather_than_orphaning_the_agent`
  and `cancelling_a_round_stops_a_clone_in_progress` spawn `assembly` with
  `h.launch_spec_for("x").await.args` instead of `job-exec` + payload; for the
  clone test, which pointed the payload at an unreachable remote, build the
  args with `LaunchSpec::for_round::<LocalRunner>` and the same remote URL.
- `tests/provisioning.rs`: the `job_exec` helper becomes `run_frames(args,
  fakes, tmp)`, running `assembly` with a `LaunchSpec`'s args plus
  `--provision-toolchain` (build the spec with `for_round::<LocalRunner>`
  and push the flag).
- `tests/payload.rs`: delete `a_payload_round_trips_through_json`,
  `job_exec_reads_the_payload_its_runner_passed`,
  `job_exec_without_a_payload_says_a_runner_starts_it`,
  `a_payload_variable_holding_something_else_is_refused` and
  `a_revised_prompt_carries_the_original_and_the_feedback`; drop `round`
  from `request`.
- `tests/docker_runner.rs`: the fake `docker`'s `run)` arm skips to the
  arguments after `assembly` and executes them:

```bash
               run) shift; while [ "$1" != assembly ]; do shift; done; shift\n\
                    echo $$ > {pid}; exec {bin} "$@" ;;\n\
```

  and every `docker.launch(&payload, ..)` becomes
  `docker.launch(&h.launch_spec_for("x").await, ..)`. `token()` returns
  secrets for both tokens. `the_agent_sees_neither_the_git_token_nor_the_payload`
  becomes `the_agent_sees_neither_token`, asserting `forge=absent`.
- `tests/kubernetes_runner.rs`: `payload_with_command_limit` goes;
  `active_deadline_secs(Some(1200))`; `job_manifest(name, "img:1", &["run".into(), "--job=1".into()], ..)`
  and its command assertion becomes `json!(["assembly", "run", "--job=1"])`;
  `a_secret_is_owned_by_its_job_and_carries_the_payload` becomes
  `a_secret_is_owned_by_its_job_and_carries_the_tokens` with
  `GH_TOKEN` in place of `ASSEMBLY_JOB`. Wherever the fake `kubectl` ran
  `job-exec` for the pod's log, it runs the manifest's command: read it
  from the manifest the fake was given on stdin
  (`jq -r '.spec.template.spec.containers[0].command[1:][]'`), or — if the
  fake does not keep the manifest — have the test pass the spec's args to
  the fake when it writes it. Every `h.payload_for("x")` becomes
  `h.launch_spec_for("x")`.
- `tests/lifecycle.rs` and `tests/cli.rs`: nothing names `job-exec`; the
  CLI test `a_remote_that_is_a_local_path_is_refused_for_a_container_runner`
  passes `--pass-env GH_TOKEN` in place of `ASSEMBLY_JOB`, sets `GH_TOKEN`,
  and expects `--pass-env GH_TOKEN`; `docker_preflight_reports_every_problem_before_allocating`
  also expects `$GH_TOKEN is not set`.
- `tests/delivery.rs`: delivery happens inside `run` now. Its `submit` tests
  (`a_failed_round_is_not_delivered`, `a_passing_revise_round_is_delivered`,
  `configured_base_reaches_the_pull_request_and_the_divergence_is_reported`,
  `a_pull_request_is_titled_and_described_from_the_job_not_from_local_commits`)
  keep passing through `submit`: the local runner's child `run` inherits the
  test's `PATH`, fake `gh` included, and its conclusion reaches `submit`'s
  output through the `PullRequestOpened` event (`pull request: …`) or the
  log. Where a test asserted `opened https://…` on `submit`'s stdout, assert
  `pull request: https://…` instead; where it asserted the `note:` about the
  differing base, assert it in `assembly logs 1` — `run` printed it to the
  round's output.

- [ ] **Step 7: The image and its smoke test**

`Dockerfile`: add `gh` to the `apt-get install` line (Debian bookworm
packages it), `&& gh --version` to the final check, update the header
comment's "`job-exec` provisions" to "`assembly run` provisions" and the
`/mise` comment's "job-exec, running as agent" to "`run`, running as agent",
and replace the last line with `CMD ["assembly", "--help"]` — a runner
always names the command.

`scripts/smoke-docker.sh` — the repository opts in with a shell agent, and
the container runs the command a runner would:

```bash
#!/usr/bin/env bash
# Runs one job in the real image, against a bare repository mounted into the
# container, with a shell one-liner standing in for the agent. Proves the
# image boots `assembly run`, runs mise, claims, commits, pushes, and prints
# frames. The runner itself is covered by `cargo test`'s fakes; this covers
# what the fakes cannot: the image.
set -euo pipefail

image="${1:?usage: smoke-docker.sh <image>}"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

git init --quiet --initial-branch=main "$work/repo"
mkdir -p "$work/repo/.assembly"
cat > "$work/repo/.assembly/config.toml" <<'TOML'
provider = "sh"
verify = "test -f smoke.txt"

[providers.sh]
cmd = "sh"
args = ["-c", "echo smoke > smoke.txt"]

[delivery]
mode = "none"
TOML
git -C "$work/repo" add -A
git -C "$work/repo" -c user.name=smoke -c user.email=smoke@localhost commit --quiet -m base
git init --quiet --bare --initial-branch=main "$work/origin.git"
git -C "$work/repo" push --quiet "$work/origin.git" main
# The container runs as uid 1000 and must be able to push.
chmod -R a+rwX "$work/origin.git"

# The mounted repository belongs to this host's uid, not the container's
# 1000, and git refuses a repository it does not own unless told it is safe.
status=0
out=$(docker run --rm \
  -e GIT_CONFIG_COUNT=1 -e GIT_CONFIG_KEY_0=safe.directory -e GIT_CONFIG_VALUE_0='*' \
  -v "$work/origin.git:/origin.git" "$image" \
  assembly run --repo=/origin.git --ref=main --prompt=smoke --frames --provision-toolchain) || status=$?
# Printed before judging, so a failed round shows the frames that say why.
echo "$out"
[ "$status" -eq 0 ] || { echo "smoke: assembly run exited $status" >&2; exit 1; }

grep -q '"t":"round_passed"' <<<"$out" || { echo "smoke: no round_passed frame" >&2; exit 1; }
grep -q smoke <<<"$(git -C "$work/origin.git" show al/job-1:smoke.txt)" || { echo "smoke: branch not pushed" >&2; exit 1; }
echo "smoke: ok"
```

`justfile`: the `smoke-docker` comment says "prove `assembly run` runs a
job end to end".

- [ ] **Step 8: Run the gate**

Run: `just check`
Expected: PASS. The count drops by the six deleted tests and rises by the
six added.

- [ ] **Step 9: Commit**

```bash
jj describe -m "refactor(runner)!: runners launch assembly run; job-exec and ASSEMBLY_JOB go

Every runner launches assembly run with plain --flag=value arguments, so
a round's command line is its reproduction and no prompt can pass for a
flag. The host claims the id, records the request and the round's start,
and collects; run reads config, runs, pushes and delivers. Container
rounds always receive GH_TOKEN as well as the git token, and the image
carries gh. A revise's agent gets only the feedback.

Tests: <count>."
jj new
```

---

### Task 8: `assembly daemon` holds a root, a runner and a socket

The daemon's skeleton, with nothing to do yet but answer: it takes an
exclusive lock on its root (one daemon per root), checks its runner and the
credentials the runner will send before it listens (Decision 10), removes a
dead daemon's socket, and serves HTTP on `<root>/daemon.sock` until SIGTERM
or Ctrl-C. The CLI's client for it lands here too, exercised by the health
route.

A dead daemon's `flock` dies with it, so a lock file left behind never blocks
the next daemon; only a live one does (Review focus 3). A socket path longer
than the platform allows is refused before anything is created, naming
`--root` (Review focus 5).

**Files:**
- Create: `src/daemon/mod.rs`, `src/daemon/root.rs`, `src/daemon/api.rs`, `src/daemon/client.rs`, `tests/daemon.rs`, `tests/support/daemon.rs`
- Modify: `Cargo.toml`, `src/lib.rs`, `src/cli.rs`, `src/main.rs`, `tests/support/mod.rs` (`pub mod daemon;`)

**Interfaces:**
- Consumes: `runner::{Runner, RunnerProblem, JobSecrets}`, `cli::RunnerArgs`.
- Produces:
  - `daemon::root::socket_path(root: &Path) -> PathBuf`; `daemon::root::hold_root(root: &Path) -> Result<RootLock, RootUnavailable>`; `RootUnavailable { Taken { root }, SocketPathTooLong { path, bytes, limit }, Unpreparable(std::io::Error) }`
  - `daemon::Daemon<R: Runner>` — `prepare(root: PathBuf, runner: R, max_jobs: usize, pass_env: &[String], env: impl Fn(&str) -> Option<String>) -> Result<Daemon<R>, Vec<RunnerProblem>>`; fields `root`, `runner`, `max_jobs`, `secrets` (private)
  - `daemon::serve<R>(daemon: Daemon<R>, lock: RootLock, shutdown: impl Future<Output = ()> + Send + 'static) -> anyhow::Result<()>`
  - `daemon::api::router<R>(daemon: Arc<Daemon<R>>) -> axum::Router`; `GET /health` → `200` with `api::Health { version: String }`
  - `daemon::client::DaemonClient` — `for_root(root: &Path) -> DaemonClient`, `health(&self) -> Result<Health, ClientError>`, and the generic `post_json<T: Serialize, U: DeserializeOwned>(&self, path: &str, body: &T) -> Result<Reply<U>, ClientError>` Task 10 uses
  - `daemon::client::ClientError { NoDaemon { root: PathBuf }, Failed(anyhow::Error) }` (`Display` says to start `assembly daemon`)
  - `daemon::client::Reply<U> { Accepted(U), Refused(api::Refused) }`; `api::Refused { summary: String, reasons: Vec<String> }`
  - `cli::Command::Daemon { runner: RunnerArgs, max_jobs: usize }`

- [ ] **Step 1: Dependencies**

`Cargo.toml` `[dependencies]`:

```toml
axum = "0.8"
http-body-util = "0.1"
hyper = { version = "1", features = ["client", "http1"] }
hyper-util = { version = "0.1", features = ["tokio"] }
nix = { version = "0.31.3", features = ["fs", "process", "signal"] }
```

(`nix` gains `fs` for `flock`; keep its version as renovate last set it.)
axum 0.8's `axum::serve` accepts a `tokio::net::UnixListener`; if the
version cargo resolves does not, serve with `hyper_util::server::conn::auto`
over `listener.accept()` instead — nothing outside `daemon::serve` changes.

- [ ] **Step 2: Write the failing tests**

`tests/support/daemon.rs`:

```rust
//! A real `assembly daemon`, started on a root the test owns and stopped
//! when the test is done with it.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

pub struct RunningDaemon {
    child: Option<Child>,
    pub root: PathBuf,
}

impl RunningDaemon {
    /// Start `assembly daemon` on `root` with `args`, and wait until it
    /// answers on its socket.
    ///
    /// # Panics
    ///
    /// If it exits, or does not answer within ten seconds.
    pub fn start(root: &Path, args: &[&str], env: &[(&str, &str)]) -> RunningDaemon {
        let mut child = Command::new(env!("CARGO_BIN_EXE_assembly"))
            .args(["daemon", "--root"])
            .arg(root)
            .args(args)
            .envs(env.iter().copied())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let socket = root.join("daemon.sock");
        let deadline = Instant::now() + Duration::from_secs(10);
        while std::os::unix::net::UnixStream::connect(&socket).is_err() {
            if let Some(status) = child.try_wait().unwrap() {
                let stderr = std::io::read_to_string(child.stderr.take().unwrap()).unwrap();
                panic!("the daemon exited {status}: {stderr}");
            }
            assert!(Instant::now() < deadline, "the daemon never answered on {}", socket.display());
            std::thread::sleep(Duration::from_millis(50));
        }
        RunningDaemon { child: Some(child), root: root.to_path_buf() }
    }

    /// SIGTERM, and wait for it to exit.
    pub fn stop(mut self) -> ExitStatus {
        let mut child = self.child.take().unwrap();
        signal(&child, nix::sys::signal::Signal::SIGTERM);
        child.wait().unwrap()
    }

    /// SIGKILL: no chance to clean anything up.
    pub fn kill(mut self) {
        let mut child = self.child.take().unwrap();
        signal(&child, nix::sys::signal::Signal::SIGKILL);
        child.wait().unwrap();
    }
}

/// SIGTERM first, so a daemon that cancels its rounds on the way out (P7)
/// takes them with it rather than leaving agents running after the test;
/// SIGKILL if it has not gone in five seconds.
impl Drop for RunningDaemon {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else { return };
        signal(&child, nix::sys::signal::Signal::SIGTERM);
        let deadline = Instant::now() + Duration::from_secs(5);
        while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        signal(&child, nix::sys::signal::Signal::SIGKILL);
        let _ = child.wait();
    }
}

fn signal(child: &Child, signal: nix::sys::signal::Signal) {
    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(child.id()).unwrap()),
        signal,
    );
}
```

(The wait loop is a test helper polling a subprocess — sequential I/O with
early exits, which is where a loop belongs.)

`tests/support/mod.rs`: `pub mod daemon;`.

`tests/daemon.rs`:

```rust
//! `assembly daemon`: one per root, checked before it listens, reachable on
//! its socket.

use assembly_line::daemon::client::{ClientError, DaemonClient};
use assert_cmd::Command;
use predicates::str::contains;
use support::daemon::RunningDaemon;

mod support;

#[tokio::test]
async fn a_daemon_answers_on_its_socket_until_stopped() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = RunningDaemon::start(tmp.path(), &[], &[]);

    let health = DaemonClient::for_root(tmp.path()).health().await.unwrap();
    assert_eq!(health.version, env!("CARGO_PKG_VERSION"));

    assert!(daemon.stop().success());
    assert!(!tmp.path().join("daemon.sock").exists(), "the socket outlived the daemon");
}

#[tokio::test]
async fn with_no_daemon_the_client_says_how_to_start_one() {
    let tmp = tempfile::tempdir().unwrap();

    let err = DaemonClient::for_root(tmp.path()).health().await.unwrap_err();

    assert!(matches!(err, ClientError::NoDaemon { .. }), "{err}");
    assert!(err.to_string().contains("assembly daemon"), "{err}");
}

#[test]
fn a_second_daemon_on_one_root_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let _first = RunningDaemon::start(tmp.path(), &[], &[]);

    Command::cargo_bin("assembly")
        .unwrap()
        .args(["daemon", "--root"])
        .arg(tmp.path())
        .assert()
        .code(2)
        .stderr(contains("another daemon holds"));
}

/// Review focus 3: a daemon that could not clean up leaves its socket and
/// lock file behind, and neither may stop the next one.
#[tokio::test]
async fn a_daemon_killed_outright_leaves_a_root_the_next_one_can_take() {
    let tmp = tempfile::tempdir().unwrap();
    RunningDaemon::start(tmp.path(), &[], &[]).kill();
    assert!(tmp.path().join("daemon.sock").exists());

    let _next = RunningDaemon::start(tmp.path(), &[], &[]);

    assert!(DaemonClient::for_root(tmp.path()).health().await.is_ok());
}

/// Review focus 5.
#[test]
fn a_root_too_deep_for_a_socket_is_refused_by_name() {
    let tmp = tempfile::tempdir().unwrap();
    let deep = tmp.path().join("d".repeat(120));

    Command::cargo_bin("assembly")
        .unwrap()
        .args(["daemon", "--root"])
        .arg(&deep)
        .assert()
        .code(2)
        .stderr(contains("--root"));
    assert!(!deep.exists(), "a refused root was created anyway");
}

#[test]
fn a_daemon_whose_runner_cannot_run_does_not_start() {
    let tmp = tempfile::tempdir().unwrap();
    let fakes = tmp.path().join("fakes");
    support::fake_cli(&fakes, "docker", "echo 'no daemon' >&2\nexit 1\n");

    Command::cargo_bin("assembly")
        .unwrap()
        .args(["daemon", "--runner", "docker", "--root"])
        .arg(tmp.path().join("root"))
        .env("PATH", format!("{}:{}", fakes.display(), std::env::var("PATH").unwrap()))
        .env_remove("ASSEMBLY_GIT_TOKEN")
        .env_remove("GH_TOKEN")
        .assert()
        .code(2)
        .stderr(contains("`docker` cannot be reached"))
        .stderr(contains("$ASSEMBLY_GIT_TOKEN is not set"))
        .stderr(contains("$GH_TOKEN is not set"));
}
```

- [ ] **Step 3: Run them to make sure they fail**

Run: `cargo test --test daemon`
Expected: FAIL to compile — `no daemon in assembly_line`.

- [ ] **Step 4: The root**

`src/daemon/root.rs`:

```rust
//! A daemon's hold on its root: one daemon per root, and the socket the CLI
//! reaches it on.

use nix::errno::Errno;
use nix::fcntl::{Flock, FlockArg};
use std::fs::File;
use std::path::{Path, PathBuf};

/// Room for a socket path, NUL excluded: `sun_path` is 104 bytes on macOS
/// and 108 on Linux, and a root must work on both.
const SOCKET_PATH_LIMIT: usize = 103;

#[must_use]
pub fn socket_path(root: &Path) -> PathBuf {
    root.join("daemon.sock")
}

fn lock_path(root: &Path) -> PathBuf {
    root.join("daemon.lock")
}

/// This daemon's exclusive hold on its root, released when dropped — or by
/// the kernel, when the process dies without dropping it.
#[derive(Debug)]
pub struct RootLock {
    _held: Flock<File>,
}

/// Why a daemon cannot have the root it was given.
#[derive(Debug)]
pub enum RootUnavailable {
    Taken { root: PathBuf },
    SocketPathTooLong { path: PathBuf, bytes: usize, limit: usize },
    Unpreparable(std::io::Error),
}

impl std::fmt::Display for RootUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Taken { root } => write!(
                f,
                "another daemon holds {} — stop it, or give this one its own --root",
                root.display()
            ),
            Self::SocketPathTooLong { path, bytes, limit } => write!(
                f,
                "the socket path {} is {bytes} bytes, past the {limit} a Unix socket allows — \
                 use a shorter --root",
                path.display()
            ),
            Self::Unpreparable(e) => write!(f, "preparing the root: {e}"),
        }
    }
}

/// Take `root` for this daemon: check its socket path fits, create it, lock
/// it, and clear a dead daemon's socket.
///
/// # Errors
///
/// When the socket path is too long (checked before anything is created),
/// another live daemon holds the root, or the root cannot be created.
pub fn hold_root(root: &Path) -> Result<RootLock, RootUnavailable> {
    let socket = socket_path(root);
    let bytes = socket.as_os_str().len();
    if bytes > SOCKET_PATH_LIMIT {
        return Err(RootUnavailable::SocketPathTooLong {
            path: socket,
            bytes,
            limit: SOCKET_PATH_LIMIT,
        });
    }
    std::fs::create_dir_all(root).map_err(RootUnavailable::Unpreparable)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path(root))
        .map_err(RootUnavailable::Unpreparable)?;
    let held = match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
        Ok(held) => held,
        Err((_, Errno::EWOULDBLOCK)) => {
            return Err(RootUnavailable::Taken { root: root.to_path_buf() });
        }
        Err((_, errno)) => return Err(RootUnavailable::Unpreparable(errno.into())),
    };
    // The lock is ours, so a socket still here is a dead daemon's.
    match std::fs::remove_file(&socket) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(RootUnavailable::Unpreparable(e))
        }
        _ => Ok(RootLock { _held: held }),
    }
}
```

- [ ] **Step 5: The daemon, its router and its client**

`src/daemon/mod.rs`:

```rust
//! The long-lived process: one root, one runner, a socket. It knows where a
//! job runs and whether it would be refused, never how it is done — that is
//! `assembly run`'s.

pub mod api;
pub mod client;
pub mod root;

use crate::runner::{JobSecrets, Runner, RunnerProblem};
use root::{RootLock, socket_path};
use std::path::PathBuf;
use std::sync::Arc;

/// A daemon whose runner and credentials have been checked.
#[derive(Debug)]
pub struct Daemon<R> {
    pub root: PathBuf,
    pub runner: R,
    pub max_jobs: usize,
    secrets: JobSecrets,
}

impl<R: Runner> Daemon<R> {
    /// Check the runner, and gather what it will send each round, before
    /// anything listens.
    ///
    /// # Errors
    ///
    /// Every reason the runner cannot run, and every credential it would
    /// send that `env` lacks.
    pub async fn prepare(
        root: PathBuf,
        runner: R,
        max_jobs: usize,
        pass_env: &[String],
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Daemon<R>, Vec<RunnerProblem>> {
        let (secrets, unsendable) = match R::RUNS_IN_A_CONTAINER {
            true => JobSecrets::from_lookup(pass_env, env),
            false => (JobSecrets::default(), Vec::new()),
        };
        let problems: Vec<RunnerProblem> = runner
            .reasons_it_cannot_run()
            .await
            .into_iter()
            .chain(unsendable)
            .collect();
        match problems.is_empty() {
            true => Ok(Daemon { root, runner, max_jobs, secrets }),
            false => Err(problems),
        }
    }

    #[must_use]
    pub fn secrets(&self) -> &JobSecrets {
        &self.secrets
    }
}

/// Serve `daemon` on its root's socket until `shutdown` resolves, then
/// remove the socket.
///
/// # Errors
///
/// When the socket cannot be bound or the server fails.
pub async fn serve<R: Runner + Send + Sync + 'static>(
    daemon: Daemon<R>,
    lock: RootLock,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let socket = socket_path(&daemon.root);
    let listener = tokio::net::UnixListener::bind(&socket)?;
    let served = axum::serve(listener, api::router(Arc::new(daemon)))
        .with_graceful_shutdown(shutdown)
        .await;
    let _ = std::fs::remove_file(&socket);
    drop(lock);
    served.map_err(Into::into)
}
```

`R: Send + Sync` holds for all three runners (they hold paths and strings).
`LocalRunner`, `DockerRunner` and `KubernetesRunner` derive `Debug`, which
`Daemon`'s derive needs.

`src/daemon/api.rs`:

```rust
//! What travels over the daemon's socket, and the routes that answer it.

use super::Daemon;
use crate::runner::Runner;
use axum::{Json, Router, routing::get};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Health {
    pub version: String,
}

/// Why the daemon would not do what it was asked, with every reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refused {
    pub summary: String,
    pub reasons: Vec<String>,
}

pub fn router<R: Runner + Send + Sync + 'static>(daemon: Arc<Daemon<R>>) -> Router {
    Router::new()
        .route("/health", get(health))
        .with_state(daemon)
}

async fn health() -> Json<Health> {
    Json(Health {
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}
```

`src/daemon/client.rs`:

```rust
//! The CLI's side of the socket: HTTP/1 over a Unix stream.

use super::api::{Health, Refused};
use super::root::socket_path;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request, StatusCode};
use hyper_util::rt::TokioIo;
use serde::{Serialize, de::DeserializeOwned};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct DaemonClient {
    root: PathBuf,
}

#[derive(Debug)]
pub enum ClientError {
    NoDaemon { root: PathBuf },
    Failed(anyhow::Error),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoDaemon { root } => write!(
                f,
                "no daemon is listening at {} — start one with `assembly daemon`",
                root.display()
            ),
            Self::Failed(e) => write!(f, "talking to the daemon: {e}"),
        }
    }
}

/// What the daemon answered a request it understood.
#[derive(Debug)]
pub enum Reply<U> {
    Accepted(U),
    Refused(Refused),
}

impl DaemonClient {
    #[must_use]
    pub fn for_root(root: &Path) -> DaemonClient {
        DaemonClient { root: root.to_path_buf() }
    }

    /// # Errors
    ///
    /// [`ClientError::NoDaemon`] when nothing listens on the root's socket.
    pub async fn health(&self) -> Result<Health, ClientError> {
        let (_, body) = self.send(Method::GET, "/health", Bytes::new()).await?;
        serde_json::from_slice(&body).map_err(|e| ClientError::Failed(e.into()))
    }

    /// POST `body` as JSON; a `422` is the daemon refusing, with reasons.
    ///
    /// # Errors
    ///
    /// When no daemon listens, or it answers something that is neither.
    pub async fn post_json<T: Serialize, U: DeserializeOwned>(
        &self,
        path: &str,
        body: &T,
    ) -> Result<Reply<U>, ClientError> {
        let json = serde_json::to_vec(body).map_err(|e| ClientError::Failed(e.into()))?;
        let (status, reply) = self.send(Method::POST, path, Bytes::from(json)).await?;
        let parsed = match status {
            StatusCode::UNPROCESSABLE_ENTITY => serde_json::from_slice(&reply).map(Reply::Refused),
            status if status.is_success() => serde_json::from_slice(&reply).map(Reply::Accepted),
            status => {
                return Err(ClientError::Failed(anyhow::anyhow!(
                    "{status}: {}",
                    String::from_utf8_lossy(&reply)
                )));
            }
        };
        parsed.map_err(|e| ClientError::Failed(e.into()))
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Bytes,
    ) -> Result<(StatusCode, Bytes), ClientError> {
        let failed = |e: &dyn std::fmt::Display| ClientError::Failed(anyhow::anyhow!("{e}"));
        let stream = tokio::net::UnixStream::connect(socket_path(&self.root))
            .await
            .map_err(|_| ClientError::NoDaemon { root: self.root.clone() })?;
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|e| failed(&e))?;
        tokio::spawn(connection);
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header(hyper::header::HOST, "assembly-daemon")
            .header(hyper::header::CONTENT_TYPE, "application/json")
            .body(Full::new(body))
            .map_err(|e| failed(&e))?;
        let response = sender.send_request(request).await.map_err(|e| failed(&e))?;
        let status = response.status();
        let body = response
            .into_body()
            .collect()
            .await
            .map_err(|e| failed(&e))?
            .to_bytes();
        Ok((status, body))
    }
}
```

`bytes` comes with hyper; if it is not re-exported where you need it, add
`bytes = "1"` to `Cargo.toml`.

`src/lib.rs`: `pub mod daemon;`.

- [ ] **Step 6: The command**

`src/cli.rs`:

```rust
    /// Run the daemon: one root, one runner, rounds launched and watched on
    /// behalf of `submit`
    Daemon {
        #[command(flatten)]
        runner: RunnerArgs,
        /// How many rounds may run at once
        #[arg(long, default_value_t = 1)]
        max_jobs: usize,
    },
```

`src/main.rs`: the arm checks `inapplicable_flags`, builds the concrete
runner exactly as `run_work_on_chosen_runner` does, and calls a generic

```rust
/// Hold the root, check the runner, and serve until SIGTERM or Ctrl-C.
async fn serve_daemon<R: Runner + Send + Sync + 'static>(
    root: PathBuf,
    runner: R,
    max_jobs: usize,
    pass_env: &[String],
) -> Result<ExitCode, String> {
    let lock = daemon::root::hold_root(&root).map_err(|e| e.to_string())?;
    let daemon = Daemon::prepare(root, runner, max_jobs, pass_env, |name| std::env::var(name).ok())
        .await
        .map_err(|problems| {
            problems.iter().for_each(|problem| eprintln!("error: {problem}"));
            "the daemon's runner cannot run".to_string()
        })?;
    eprintln!("listening on {}", daemon::root::socket_path(&daemon.root).display());
    daemon::serve(daemon, lock, termination_requested())
        .await
        .map(|()| ExitCode::SUCCESS)
        .map_err(|e| e.to_string())
}

/// Resolves on SIGTERM or Ctrl-C.
async fn termination_requested() {
    use tokio::signal::unix::{SignalKind, signal};
    match signal(SignalKind::terminate()) {
        Ok(mut terminate) => {
            tokio::select! {
                _ = terminate.recv() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
        }
        Err(_) => {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}
```

Both `submit` (until Task 10) and `daemon` build a runner from `RunnerArgs`,
and each then calls a function generic over `R: Runner`. Build it once, into
an enum in `main.rs` — building the concrete runner is main's job — and match
on it at each call site:

```rust
/// The runner `RunnerArgs` chose, built.
enum ChosenRunner {
    Local(LocalRunner),
    Docker(DockerRunner),
    K8s(KubernetesRunner),
}

fn chosen_runner(args: &RunnerArgs) -> Result<ChosenRunner, String> {
    if let Some(inapplicable) = args.inapplicable_flags() {
        return Err(inapplicable.to_string());
    }
    let image = || args.image.clone().unwrap_or_else(runner::published_image);
    match args.runner {
        RunnerKind::Local => LocalRunner::current_binary()
            .map(ChosenRunner::Local)
            .map_err(|e| e.to_string()),
        RunnerKind::Docker => Ok(ChosenRunner::Docker(DockerRunner::new(image()))),
        // clap has already required a namespace for k8s, so the default is
        // never taken.
        RunnerKind::K8s => Ok(ChosenRunner::K8s(KubernetesRunner::new(
            image(),
            args.namespace.clone().unwrap_or_default(),
            args.context.clone(),
        ))),
    }
}
```

`run_work_on_chosen_runner` becomes a `match chosen_runner(&args)? { … }`
calling `run_work` in each arm, and the daemon arm likewise calls
`serve_daemon`.

- [ ] **Step 7: Run the gate**

Run: `just check`
Expected: PASS, with `tests/daemon.rs`'s six tests added.

- [ ] **Step 8: Commit**

```bash
jj describe -m "feat(daemon): assembly daemon holds a root, a runner and a socket

One daemon per root, by flock, so a dead daemon's lock never blocks the
next and its stale socket is cleared. The runner and the credentials it
will send are checked before it listens. HTTP over <root>/daemon.sock,
with a health route and the CLI's client. A socket path too long for
the platform is refused by naming --root.

Tests: <count>."
jj new
```

---

### Task 9: The test harness runs a job the way `assembly run` does

`tests/support`'s `Harness::run_job` drives `lifecycle` — the host half that
Task 10 deletes. Most tests that use it are about what a job *does*, which is
`run`'s now, so the harness runs a job in-process with `run::prepare_run`
and `ReadyJob::run`, collecting frames into memory. No runner, no daemon: a
test of what a round does should not pay for either.

**Files:**
- Modify: `tests/support/mod.rs`, `tests/round.rs`, `tests/collect.rs`, `tests/docker_runner.rs`, `tests/kubernetes_runner.rs`
- No production code changes.

**Interfaces:**
- Consumes: `run::{RunRequest, RunRefused, prepare_run, ReadyJob}`, `frame::{FrameWriter, StreamPosition, Routed}`.
- Produces (test support): `Harness::run_job(&self, prompt) -> Outcome`, `Harness::run_job_from(&self, prompt, base_ref) -> Outcome`, `Harness::revise_job(&self, job_id: JobId, prompt) -> Outcome`, `Harness::refusal_to_start(&self, prompt, provider: Option<&str>) -> RunRefused`. `Harness::prepare_job` and `Harness::run(prepared)` are deleted.

- [ ] **Step 1: Rewrite the harness's job methods**

In `tests/support/mod.rs`, replace `run_job`, `prepare_job`, `run_job_from`,
`revise_job`, `refusal_to_start`, `start` and `run` with:

```rust
    /// Run one job against this repository, with `prompt`, from `main` —
    /// in this process, as `assembly run` does.
    pub async fn run_job(&self, prompt: &str) -> Outcome {
        self.run(self.request(prompt, None, None)).await
    }

    /// Run one job cut from a named ref rather than `main`.
    pub async fn run_job_from(&self, prompt: &str, base_ref: &str) -> Outcome {
        self.run(self.request(prompt, None, Some(base_ref))).await
    }

    /// Another round on job `job_id`, continuing its branch.
    pub async fn revise_job(&self, job_id: JobId, prompt: &str) -> Outcome {
        self.run(RunRequest {
            job: Some(job_id.into()),
            ..self.request(prompt, None, None)
        })
        .await
    }

    /// Why a job with `provider` would not start: a refusal before anything
    /// was claimed, as distinct from a job that failed.
    pub async fn refusal_to_start(&self, prompt: &str, provider: Option<&str>) -> RunRefused {
        match prepare_run(
            self.request(prompt, provider, None),
            &self.scratch_root(),
            &CancellationToken::new(),
        )
        .await
        {
            Ok(_) => panic!("the job was ready to run, not refused"),
            Err(refused) => refused,
        }
    }

    fn request(&self, prompt: &str, provider: Option<&str>, base_ref: Option<&str>) -> RunRequest {
        RunRequest {
            repo: Some(self.repo.to_string_lossy().into_owned()),
            base_ref: base_ref.map(str::to_string),
            prompt: Some(prompt.to_string()),
            provider: provider.map(str::to_string),
            ..RunRequest::default()
        }
    }

    async fn run(&self, request: RunRequest) -> Outcome {
        let cancel = CancellationToken::new();
        let ready = match prepare_run(request, &self.scratch_root(), &cancel).await {
            Ok(ready) => ready,
            Err(refused) => panic!(
                "the job was refused: {refused} {:?}",
                refused.itemized_reasons()
            ),
        };
        let frames = FrameWriter::new(Vec::new());
        let conclusion = ready.run(&frames, cancel).await.unwrap();
        let output: String = String::from_utf8(frames.copy_of_sink())
            .unwrap()
            .lines()
            .filter_map(|line| match StreamPosition::default().route(line).1 {
                Routed::Output(text) => Some(format!("{text}\n")),
                Routed::Event { .. } | Routed::AlreadyCollected => None,
            })
            .collect();

        Outcome {
            passed: conclusion.verdict.passed(),
            job_id: conclusion.job,
            state: conclusion.report.state,
            events: frames.events_so_far().into_iter().map(|e| e.kind).collect(),
            output,
        }
    }
```

The harness's `runner` field and `assembly_with_scratch_under` stay:
`tests/collect.rs` and the runner tests still hand rounds to the local
runner through `launch_spec_for`. `Harness::with_config` keeps `[delivery]
mode = "none"`, so no in-process job reaches for `gh`.

- [ ] **Step 2: Fix the tests that named the old types**

- `tests/round.rs`: `a_provider_the_repository_never_declared_stops_the_job_before_it_starts`
  and `an_unparseable_max_duration_stops_the_job_before_it_starts` match
  `RunRefused::ConfigNotRunnable(errors)` instead of
  `Refusal::ConfigNotRunnable(errors)`; import `assembly_line::run::RunRefused`.
  Tests that asserted `EventKind::RoundStarted` in an `Outcome`'s events
  drop that assertion: `run` numbers no rounds (P1), and the host that
  does is not in this path.
- Every other `Harness` user compiles unchanged.

- [ ] **Step 3: Run the gate**

Run: `just check`
Expected: PASS, count unchanged. Run it twice: the in-process harness changes
which process spawns the agents, and a flaky ordering shows up as a second
run disagreeing with the first.

- [ ] **Step 4: Commit**

```bash
jj describe -m "test: the harness runs a job the way assembly run does

Tests of what a job does run it in-process with run's own prepare and
run, collecting frames in memory, instead of through the host lifecycle
the next change deletes.

Tests: <count>."
jj new
```

---

### Task 10: `submit` queues a job with the daemon, preflighted and claimed

`submit` becomes a client: it resolves the checkout to its remote URL and
the ref to start from, prints the "your local ref differs" note, and posts
the job to the daemon — then exits, `job 7 queued on al/job-7`, or 2 with
every reason for a refusal. The daemon does what `lifecycle` did, in its own
bare cache so the user's `.git` is never written (Decision 16, the tightened
invariant): pin the base, validate the config at that commit, check the
remote against its runner, claim `al/job-N`, record `RoundRequested`, queue
the job. A dispatcher launches queued rounds oldest first, never more than
`--max-jobs` at once, and collects each into the job's log.

`lifecycle` is deleted: its host half is here, its reading half moves to
`locate`. The runner flags leave `submit` for good.

Stopping the daemon cancels its running rounds and waits for their verdicts
(P7), so no log is left mid-round; Task 12 replaces that with reattach.
Queued jobs stay queued in their logs across a restart, but nothing requeues
them until Task 12's startup fold.

**Files:**
- Create: `src/daemon/submit.rs`, `src/daemon/dispatch.rs`, `src/submission.rs`, `tests/daemon_jobs.rs`, `tests/submission.rs`, `tests/locate.rs`, `tests/fixtures/counting-agent.sh`
- Modify: `Cargo.toml` (`tokio-util` `rt` feature), `src/daemon/mod.rs`, `src/daemon/api.rs`, `src/git.rs`, `src/locate.rs`, `src/report.rs`, `src/state.rs`, `src/cli.rs`, `src/main.rs`, `src/lib.rs`
- Delete: `src/lifecycle.rs`, `tests/lifecycle.rs`
- Test: `tests/cli.rs`, `tests/delivery.rs`, `tests/support/daemon.rs`

**Interfaces:**
- Consumes: `claim::claim_job`, `config::RepoConfig`, `collect::{collect, record_launch_failure}`, `runner::{LaunchSpec, reasons_a_container_cannot_run}`, `paths::RepoKey`, `daemon::{Daemon, client::DaemonClient}`.
- Produces:
  - `daemon::api::Submission { remote_url: String, base_ref: Option<String>, job: Option<u64>, prompt: String, provider: Option<String> }`
  - `daemon::api::Queued { job: u64, branch: String, warnings: Vec<String> }` with `Display` (`job 7 queued on al/job-7`)
  - `POST /jobs` → `200 Queued` or `422 Refused`
  - `daemon::Serving<R> { daemon: Daemon<R>, repos: RepoLocks, queue: JobQueue }`
  - `daemon::dispatch::{JobAddress, JobQueue, RepoLocks, dispatch_until_closed}`; `JobAddress { key: RepoKey, jobs_dir: PathBuf, id: JobId }` with `paths()`; `JobQueue::enqueue(&self, JobAddress)`, `JobQueue::stop_rounds_and_wait(&self)`
  - `daemon::submit::accept<R: Runner>(serving: &Serving<R>, submission: Submission) -> Result<Queued, Refused>`
  - `submission::{SubmitRequest, PreparedSubmission, LocalRefDiffers, prepare_submission}`
  - `git::init_bare_if_absent(path: &Path) -> anyhow::Result<()>`, `git::sha_on_remote(repo: impl AsRef<Path>, remote: &str, git_ref: &str) -> anyhow::Result<Option<String>>`
  - `JobState::Queued` (label `queued`); `JobReport::latest_prompt: Option<String>`
  - `locate::report_for_job(root, job_id: Option<u64>, repo) -> anyhow::Result<JobReport>`, `locate::output_log_of(root, job_id: u64, repo) -> anyhow::Result<PathBuf>` (moved from `lifecycle`)
  - `cli::Command::Submit { prompt, prompt_file, repo, base_ref, provider, job }` (no `runner`)
  - test support: `support::daemon::wait_for_verdict(job_dir: &Path) -> JobReport`

- [ ] **Step 1: Write the failing tests**

`tests/fixtures/counting-agent.sh`:

```bash
#!/usr/bin/env bash
# Records how many copies of itself are live at once, in $PROBE_DIR, so a
# test can prove a concurrency cap by observation rather than by trusting
# the scheduler's own counters.
set -euo pipefail
mkdir -p "$PROBE_DIR"
touch "$PROBE_DIR/live.$$"
ls "$PROBE_DIR" | grep -c '^live\.' >> "$PROBE_DIR/seen" || true
sleep 1
rm "$PROBE_DIR/live.$$"
printf '%s\n' "$1" > agent-output.txt
```

`tests/support/daemon.rs` gains:

```rust
/// Fold `job_dir`'s log until its round has a verdict.
///
/// # Panics
///
/// If there is none within thirty seconds.
pub fn wait_for_verdict(job_dir: &Path) -> JobReport {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let events = EventLog::read(job_dir.join("events.jsonl")).unwrap();
        let report = JobReport::from_events(0, &events);
        if matches!(report.state, JobState::Passed | JobState::Failed) {
            return report;
        }
        assert!(Instant::now() < deadline, "no verdict in {}: {events:?}", job_dir.display());
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Fold `job_dir`'s log until its round is running.
pub fn wait_until_running(job_dir: &Path) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while JobReport::from_events(0, &EventLog::read(job_dir.join("events.jsonl")).unwrap()).state
        != JobState::Running
    {
        assert!(Instant::now() < deadline, "{} never started", job_dir.display());
        std::thread::sleep(Duration::from_millis(100));
    }
}
```

`tests/daemon_jobs.rs`:

```rust
//! Jobs through the daemon: submitted, preflighted, claimed, queued under a
//! cap, run on the daemon's runner, and read back with status and logs.

use assembly_line::config::REPO_CONFIG_PATH;
use assembly_line::paths::RepoKey;
use assembly_line::state::JobState;
use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use std::path::{Path, PathBuf};
use support::daemon::{RunningDaemon, wait_for_verdict, wait_until_running};

mod support;

/// An opted-in repository published to a bare origin, and a daemon on a
/// root beside it running rounds on the local runner.
///
/// `daemon` is declared first so it is dropped — stopped — before `tmp`
/// deletes the root and scratch it is using.
struct Fixture {
    daemon: Option<RunningDaemon>,
    tmp: tempfile::TempDir,
    repo: PathBuf,
    origin: PathBuf,
}

impl Fixture {
    async fn running(script: &str, daemon_args: &[&str], daemon_env: &[(&str, &str)]) -> Self {
        let fx = Self::repository(&format!(
            "verify = \"true\"\n{}",
            support::config_running(script)
        ))
        .await;
        fx.with_daemon(daemon_args, daemon_env)
    }

    async fn repository(config: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        support::init_git_repo(&repo).await;
        std::fs::create_dir_all(repo.join(".assembly")).unwrap();
        std::fs::write(
            repo.join(REPO_CONFIG_PATH),
            format!("{config}\n[delivery]\nmode = \"none\"\n"),
        )
        .unwrap();
        support::commit_all(&repo, "opt in").await.unwrap().unwrap();
        let origin = tmp.path().join("origin.git");
        support::add_origin(&repo, &origin).await;
        support::publish_main(&repo).await;
        Fixture { tmp, repo, origin, daemon: None }
    }

    fn with_daemon(mut self, args: &[&str], env: &[(&str, &str)]) -> Self {
        let scratch = self.tmp.path().join("scratch");
        let path = support::path_where_gh_refuses();
        let env: Vec<(&str, &str)> = [("TMPDIR", scratch.to_str().unwrap()), ("PATH", path.as_str())]
            .into_iter()
            .chain(env.iter().copied())
            .collect();
        self.daemon = Some(RunningDaemon::start(&self.root(), args, &env));
        self
    }

    fn root(&self) -> PathBuf {
        self.tmp.path().join("root")
    }

    fn job_dir(&self, id: u64) -> PathBuf {
        RepoKey::from_remote_url(self.origin.to_str().unwrap())
            .unwrap()
            .jobs_dir(&self.root())
            .join(id.to_string())
    }

    fn assembly(&self) -> Command {
        let mut cmd = Command::cargo_bin("assembly").unwrap();
        cmd.current_dir(&self.repo)
            .env("PATH", support::path_where_gh_refuses())
            .env("ASSEMBLY_ROOT", self.root());
        cmd
    }

    fn on_origin(&self, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.origin)
            .args(args)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
}

#[tokio::test]
async fn a_submitted_job_is_queued_then_run_by_the_daemon() {
    let fx = Fixture::running("fake-agent.sh", &[], &[]).await;

    fx.assembly()
        .args(["submit", "--prompt", "do the thing"])
        .assert()
        .success()
        .stdout(contains("job 1 queued on al/job-1"));
    let report = wait_for_verdict(&fx.job_dir(1));

    assert_eq!(report.state, JobState::Passed);
    fx.assembly()
        .arg("status")
        .assert()
        .success()
        .stdout(contains("job 1: passed (round 1").and(contains("branch: al/job-1")));
    fx.assembly()
        .args(["logs", "1"])
        .assert()
        .success()
        .stdout(contains("fake-agent: do the thing"));
}

#[tokio::test]
async fn submit_with_no_daemon_says_how_to_start_one() {
    let fx = Fixture::repository(&support::config_running("fake-agent.sh")).await;

    fx.assembly()
        .args(["submit", "--prompt", "x"])
        .assert()
        .code(2)
        .stderr(contains("assembly daemon"));
}

/// Preflight at submit: every reason at once, and nothing claimed.
#[tokio::test]
async fn a_config_that_cannot_run_is_refused_at_submit_and_claims_nothing() {
    let fx = Fixture::repository("provider = \"ghost\"\nmax_duration = \"soon\"")
        .await
        .with_daemon(&[], &[]);

    fx.assembly()
        .args(["submit", "--prompt", "x"])
        .assert()
        .code(2)
        .stderr(contains("'ghost'").and(contains("max_duration 'soon'")));

    assert_eq!(fx.on_origin(&["branch", "--list", "al/job-*"]), "");
    assert!(!fx.job_dir(1).exists());
}

/// Review focus 4.
#[tokio::test]
async fn two_submits_at_once_to_one_repository_get_two_jobs() {
    let fx = Fixture::running("fake-agent.sh", &["--max-jobs", "2"], &[]).await;

    let submits: Vec<std::process::Child> = (0..2)
        .map(|i| {
            std::process::Command::new(env!("CARGO_BIN_EXE_assembly"))
                .current_dir(&fx.repo)
                .env("ASSEMBLY_ROOT", fx.root())
                .args(["submit", "--prompt", &format!("job {i}")])
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let outputs: Vec<std::process::Output> =
        submits.into_iter().map(|child| child.wait_with_output().unwrap()).collect();

    assert!(outputs.iter().all(|out| out.status.success()), "{outputs:?}");
    assert_eq!(wait_for_verdict(&fx.job_dir(1)).state, JobState::Passed);
    assert_eq!(wait_for_verdict(&fx.job_dir(2)).state, JobState::Passed);
}

/// The cap, proven by observation: every copy of the probe agent records how
/// many copies were live when it started.
#[tokio::test]
async fn the_daemon_never_runs_more_rounds_at_once_than_its_cap() {
    let probe = tempfile::tempdir().unwrap();
    let probe_dir = probe.path().to_str().unwrap();
    let fx = Fixture::running(
        "counting-agent.sh",
        &["--max-jobs", "2"],
        &[("PROBE_DIR", probe_dir)],
    )
    .await;

    (0..5).for_each(|i| {
        fx.assembly()
            .args(["submit", "--prompt", &format!("job {i}")])
            .assert()
            .success();
    });
    (1..=5).for_each(|id| {
        assert_eq!(wait_for_verdict(&fx.job_dir(id)).state, JobState::Passed);
    });

    let seen: Vec<u32> = std::fs::read_to_string(probe.path().join("seen"))
        .unwrap()
        .lines()
        .map(|n| n.trim().parse().unwrap())
        .collect();
    assert_eq!(seen.len(), 5);
    assert_eq!(seen.iter().max(), Some(&2), "{seen:?}");
}

#[tokio::test]
async fn a_revise_through_the_daemon_is_the_jobs_next_round() {
    let fx = Fixture::running("revising-agent.sh", &[], &[]).await;
    fx.assembly().args(["submit", "--prompt", "hi"]).assert().success();
    wait_for_verdict(&fx.job_dir(1));

    fx.assembly()
        .args(["submit", "--job", "1", "--prompt", "more"])
        .assert()
        .success()
        .stdout(contains("job 1 queued"));
    wait_for_verdict(&fx.job_dir(1));

    fx.assembly()
        .args(["status", "1"])
        .assert()
        .success()
        .stdout(contains("job 1: passed (round 2"));
    assert_eq!(fx.on_origin(&["show", "al/job-1:rounds.txt"]), "hi\nmore");
}

#[tokio::test]
async fn a_revise_of_a_job_still_running_is_refused() {
    let fx = Fixture::running("sleeping-agent.sh", &[], &[]).await;
    fx.assembly().args(["submit", "--prompt", "x"]).assert().success();
    wait_until_running(&fx.job_dir(1));

    fx.assembly()
        .args(["submit", "--job", "1", "--prompt", "more"])
        .assert()
        .code(2)
        .stderr(contains("already queued or running"));
}

#[tokio::test]
async fn revising_a_job_that_does_not_exist_is_refused() {
    let fx = Fixture::running("fake-agent.sh", &[], &[]).await;

    fx.assembly()
        .args(["submit", "--job", "9", "--prompt", "more"])
        .assert()
        .code(2)
        .stderr(contains("no such job: 9"));
}

/// The tightened invariant: the daemon pins and claims in its own cache;
/// `submit` only lists the remote. Nothing is fetched into the user's `.git`.
#[tokio::test]
async fn the_users_repository_gains_nothing_from_a_job() {
    let fx = Fixture::running("fake-agent.sh", &[], &[]).await;
    let refs_before = git_in(&fx.repo, &["for-each-ref"]);

    fx.assembly().args(["submit", "--prompt", "x"]).assert().success();
    wait_for_verdict(&fx.job_dir(1));

    assert!(!fx.repo.join(".git/FETCH_HEAD").exists(), "something fetched into the user's .git");
    assert_eq!(git_in(&fx.repo, &["for-each-ref"]), refs_before);
    assert_eq!(git_in(&fx.repo, &["status", "--porcelain"]), "");
}

/// Until reattach lands, stopping the daemon ends its rounds — and says so.
#[tokio::test]
async fn stopping_the_daemon_cancels_its_rounds_and_records_why() {
    let mut fx = Fixture::running("sleeping-agent.sh", &[], &[]).await;
    fx.assembly().args(["submit", "--prompt", "x"]).assert().success();
    wait_until_running(&fx.job_dir(1));

    assert!(fx.daemon.take().unwrap().stop().success());

    let report = wait_for_verdict(&fx.job_dir(1));
    assert_eq!(report.state, JobState::Failed);
    assert!(report.detail.unwrap_or_default().contains("cancelled"));
}

/// A token exported for container runners must not take over a local round,
/// which uses whatever git already authenticates with on this machine.
#[tokio::test]
async fn the_local_runner_keeps_the_hosts_credentials_even_with_a_token_exported() {
    let fx = Fixture::running(
        "credential-reporting-agent.sh",
        &[],
        &[("ASSEMBLY_GIT_TOKEN", "t")],
    )
    .await;

    fx.assembly().args(["submit", "--prompt", "x"]).assert().success();
    wait_for_verdict(&fx.job_dir(1));

    fx.assembly()
        .args(["logs", "1"])
        .assert()
        .success()
        .stdout(contains("helpers:").and(contains("x-access-token").not()));
}

#[tokio::test]
async fn unpushed_local_work_is_pointed_out_and_the_remotes_ref_is_used() {
    let fx = Fixture::running("fake-agent.sh", &[], &[]).await;
    let remotes = fx.on_origin(&["rev-parse", "main"]);
    std::fs::write(fx.repo.join("unpushed.txt"), "mine\n").unwrap();
    support::commit_all(&fx.repo, "unpushed").await.unwrap().unwrap();

    fx.assembly()
        .args(["submit", "--prompt", "x"])
        .assert()
        .success()
        .stdout(contains("push first"));
    wait_for_verdict(&fx.job_dir(1));

    assert_eq!(fx.on_origin(&["rev-parse", "al/job-1^"]), remotes);
}

/// `--repo` finds a job's state from anywhere, and nothing is written to
/// the repository the command was typed in.
#[tokio::test]
async fn a_job_submitted_elsewhere_is_found_by_pointing_the_read_commands_at_it() {
    let fx = Fixture::running("fake-agent.sh", &[], &[]).await;
    let standing_in = support::repo_with_initial_commit().await;
    let at = fx.repo.to_str().unwrap();
    let from_elsewhere = || {
        let mut cmd = Command::cargo_bin("assembly").unwrap();
        cmd.current_dir(standing_in.path()).env("ASSEMBLY_ROOT", fx.root());
        cmd
    };

    from_elsewhere().args(["submit", "--repo", at, "--prompt", "x"]).assert().success();
    wait_for_verdict(&fx.job_dir(1));

    from_elsewhere()
        .args(["status", "--repo", at])
        .assert()
        .success()
        .stdout(contains("job 1: passed"));
    from_elsewhere()
        .args(["logs", "1", "--repo", at])
        .assert()
        .success()
        .stdout(contains("fake-agent"));
    assert!(!standing_in.path().join(".assembly").exists());
}

fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}
```

(`the_daemon_never_runs_more_rounds_at_once_than_its_cap` submits the five
jobs one after another; `for_each` is fine here because each submit is a
blocking assertion.)

Add a container-runner refusal: a daemon on `--runner docker` with a fake
`docker` whose `version` succeeds, both tokens set, refuses a submit whose
remote is the fixture's local-path origin:

```rust
#[tokio::test]
async fn a_remote_that_is_a_local_path_is_refused_by_a_container_daemon() {
    let fx = Fixture::repository(&support::config_running("fake-agent.sh")).await;
    let fakes = fx.tmp.path().join("fakes");
    support::fake_cli(&fakes, "docker", "echo 27.0.0\n");
    let path = format!("{}:{}", fakes.display(), std::env::var("PATH").unwrap());
    let fx = fx.with_daemon(
        &["--runner", "docker"],
        &[("PATH", path.as_str()), ("ASSEMBLY_GIT_TOKEN", "t"), ("GH_TOKEN", "t")],
    );

    fx.assembly()
        .args(["submit", "--prompt", "x"])
        .assert()
        .code(2)
        .stderr(contains("is a path on this machine"));
}
```

(`with_daemon` puts its own `PATH` first in the env list; the later
`("PATH", …)` wins because `Command::envs` applies them in order.)

`tests/submission.rs`, from the two `tests/lifecycle.rs` tests that were
about the CLI's side:

```rust
use assembly_line::git;
use assembly_line::submission::{LocalRefDiffers, SubmitRequest, prepare_submission};
use support::{Harness, commit_all};

mod support;

fn submitting(h: &Harness) -> SubmitRequest {
    SubmitRequest {
        prompt: Some("x".into()),
        repo: Some(h.repo.to_string_lossy().into_owned()),
        ..SubmitRequest::default()
    }
}

#[tokio::test]
async fn unpushed_local_work_is_noted_against_the_remotes_commit() {
    let h = Harness::new().await;
    let remotes = git::sha_at_ref(&h.origin, "main").await.unwrap();
    std::fs::write(h.repo.join("unpushed.txt"), "mine\n").unwrap();
    commit_all(&h.repo, "unpushed").await.unwrap().unwrap();

    let prepared = prepare_submission(submitting(&h)).await.unwrap();

    assert_eq!(
        prepared.notes,
        [LocalRefDiffers { base_ref: "main".into(), remote: "origin".into(), remote_sha: remotes }]
    );
    assert_eq!(prepared.submission.base_ref.as_deref(), Some("main"));
}

#[tokio::test]
async fn a_repository_with_no_remote_cannot_be_submitted() {
    let repo = support::repo_with_initial_commit().await;

    let err = prepare_submission(SubmitRequest {
        prompt: Some("x".into()),
        repo: Some(repo.path().to_string_lossy().into_owned()),
        ..SubmitRequest::default()
    })
    .await
    .unwrap_err();

    assert!(err.to_string().contains("no 'origin' remote"), "{err}");
}

#[tokio::test]
async fn a_revise_names_no_ref_and_leaves_the_base_to_the_job() {
    let h = Harness::new().await;

    let prepared = prepare_submission(SubmitRequest { job: Some(3), ..submitting(&h) })
        .await
        .unwrap();

    assert_eq!(prepared.submission.base_ref, None);
    assert_eq!(prepared.submission.job, Some(3));
    assert!(prepared.notes.is_empty());
}
```

`tests/locate.rs`, from the four `tests/lifecycle.rs` tests about reading
state — `status_without_a_job_id_reports_the_latest_job`,
`status_in_a_repository_with_no_jobs_says_so`,
`a_job_that_has_captured_nothing_has_no_log_to_show` and
`a_job_that_has_captured_output_names_its_log` — moved as they are, calling
`locate::report_for_job(&h.root, …)` and `locate::output_log_of(&h.root, …)`,
with `job_in` (the helper that makes a job directory and its
`RoundRequested`) moved with them.

- [ ] **Step 2: Run them to make sure they fail**

Run: `cargo test --test daemon_jobs --test submission`
Expected: FAIL to compile — `no submission in assembly_line`, `Submit` has a `runner` field.

- [ ] **Step 3: The fold learns the queue**

`src/state.rs`: add `Queued` between `Pending` and `Running`, labelled
`queued`, documented "asked for, and waiting for a slot to run in".
`src/report.rs`: `RoundRequested` sets `state: JobState::Queued` and
`latest_prompt: Some(prompt.clone())`; add `latest_prompt` to `JobReport`
("What the job's latest round was asked to do").

- [ ] **Step 4: Git for the cache and the note**

`src/git.rs`:

```rust
/// A bare repository at `path`, made if nothing is there yet — the daemon's
/// cache for one remote, which never has a working tree.
pub async fn init_bare_if_absent(path: &Path) -> anyhow::Result<()> {
    if path.join("HEAD").exists() {
        return Ok(());
    }
    std::fs::create_dir_all(path)?;
    run_expecting_success(path, &["init", "--bare", "--quiet"], "init --bare")
        .await
        .map(|_| ())
}

/// The commit `git_ref` names on `remote`, asked without fetching anything
/// into `repo` — or `None` when the remote has no such ref.
pub async fn sha_on_remote(
    repo: impl AsRef<Path>,
    remote: &str,
    git_ref: &str,
) -> anyhow::Result<Option<String>> {
    let listed = run_expecting_success(repo, &["ls-remote", remote, git_ref], "ls-remote").await?;
    Ok(listed
        .lines()
        .find_map(|line| line.split_once('\t'))
        .map(|(sha, _)| sha.to_string()))
}
```

- [ ] **Step 5: The daemon's side of a submit**

`Cargo.toml`: `tokio-util = { version = "0.7", features = ["rt"] }` (for
`TaskTracker`).

`src/daemon/api.rs` — add the wire types and the route:

```rust
/// A job for the daemon: a new one, or with `job`, another round of one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Submission {
    pub remote_url: String,
    /// The ref a new job starts from. A revise names none: it starts from
    /// its job's own base.
    pub base_ref: Option<String>,
    pub job: Option<u64>,
    pub prompt: String,
    pub provider: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Queued {
    pub job: u64,
    pub branch: String,
    /// Settings worth flagging that did not stop the job.
    pub warnings: Vec<String>,
}

impl std::fmt::Display for Queued {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "job {} queued on {}", self.job, self.branch)
    }
}
```

```rust
pub fn router<R: Runner + Send + Sync + 'static>(serving: Arc<Serving<R>>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/jobs", post(submit_job::<R>))
        .with_state(serving)
}

async fn submit_job<R: Runner + Send + Sync + 'static>(
    State(serving): State<Arc<Serving<R>>>,
    Json(submission): Json<Submission>,
) -> Result<Json<Queued>, (StatusCode, Json<Refused>)> {
    submit::accept(&serving, submission)
        .await
        .map(Json)
        .map_err(|refused| (StatusCode::UNPROCESSABLE_ENTITY, Json(refused)))
}
```

`src/daemon/dispatch.rs`:

```rust
//! The queue, the cap, and one round at a time per job: launch it on the
//! daemon's runner and collect it into the job's log.

use super::Serving;
use crate::collect::{collect, record_launch_failure};
use crate::config::RepoConfig;
use crate::event::{EventKind, EventLog};
use crate::job::JobId;
use crate::paths::{JobPaths, RepoKey};
use crate::report::JobReport;
use crate::runner::{LaunchSpec, Runner};
use crate::state::JobState;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::{OwnedMutexGuard, mpsc, watch};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// Where one job's state lives, which is all the daemon needs to find it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JobAddress {
    pub key: RepoKey,
    pub jobs_dir: PathBuf,
    pub id: JobId,
}

impl JobAddress {
    #[must_use]
    pub fn paths(&self) -> JobPaths {
        JobPaths {
            id: self.id,
            dir: self.jobs_dir.join(self.id.to_string()),
        }
    }

    fn report(&self) -> anyhow::Result<JobReport> {
        let events = EventLog::read(self.paths().events())?;
        Ok(JobReport::from_events(self.id.into(), &events))
    }
}

/// One lock per repository cache (P5): two fetches into one bare repository
/// race on `FETCH_HEAD`.
#[derive(Debug, Default)]
pub struct RepoLocks {
    held: Mutex<HashMap<RepoKey, Arc<tokio::sync::Mutex<()>>>>,
}

impl RepoLocks {
    /// # Panics
    ///
    /// If a holder panicked while registering a lock.
    pub async fn hold(&self, key: &RepoKey) -> OwnedMutexGuard<()> {
        let lock = self
            .held
            .lock()
            .expect("repository lock table")
            .entry(key.clone())
            .or_default()
            .clone();
        lock.lock_owned().await
    }
}

/// Jobs waiting for a slot, the rounds running now, and a way to stop them.
#[derive(Debug)]
pub struct JobQueue {
    pending: mpsc::UnboundedSender<JobAddress>,
    running: watch::Sender<usize>,
    rounds: TaskTracker,
    stopping: CancellationToken,
    cancels: Mutex<HashMap<JobAddress, CancellationToken>>,
}

impl JobQueue {
    /// The queue, and the receiving end its dispatcher takes.
    #[must_use]
    pub fn new() -> (JobQueue, mpsc::UnboundedReceiver<JobAddress>) {
        let (pending, receiving) = mpsc::unbounded_channel();
        (
            JobQueue {
                pending,
                running: watch::Sender::new(0),
                rounds: TaskTracker::new(),
                stopping: CancellationToken::new(),
                cancels: Mutex::new(HashMap::new()),
            },
            receiving,
        )
    }

    /// Put a job in line behind every job already waiting.
    pub fn enqueue(&self, address: JobAddress) {
        // Only fails once the dispatcher is gone, which is shutdown.
        let _ = self.pending.send(address);
    }

    /// Cancel every running round and wait until each has its verdict.
    pub async fn stop_rounds_and_wait(&self) {
        self.stopping.cancel();
        self.rounds.close();
        self.rounds.wait().await;
    }
}

/// Launch queued jobs, oldest first, whenever fewer than `max_jobs` rounds
/// are running, until the queue closes or the daemon stops.
///
/// A loop, not a stream combinator: each job waits for a slot before the
/// next one is looked at, which is what keeps the order.
pub async fn dispatch_until_closed<R: Runner + Send + Sync + 'static>(
    serving: Arc<Serving<R>>,
    mut pending: mpsc::UnboundedReceiver<JobAddress>,
) {
    let mut running = serving.queue.running.subscribe();
    while let Some(address) = pending.recv().await {
        let slot = running.wait_for(|n| *n < serving.daemon.max_jobs);
        let stopped = tokio::select! {
            waited = slot => waited.is_err(),
            () = serving.queue.stopping.cancelled() => true,
        };
        if stopped {
            return;
        }
        // A job cancelled while it waited is no longer queued.
        if !matches!(address.report().map(|r| r.state), Ok(JobState::Queued)) {
            continue;
        }
        let cancel = serving.queue.stopping.child_token();
        serving
            .queue
            .cancels
            .lock()
            .expect("cancel table")
            .insert(address.clone(), cancel.clone());
        serving.queue.running.send_modify(|n| *n += 1);
        let serving = Arc::clone(&serving);
        let tracker = serving.queue.rounds.clone();
        tracker.spawn(async move {
            if let Err(e) = run_queued_round(&serving, &address, cancel).await {
                tracing::error!("job {} in {}: {e:#}", address.id, address.key);
            }
            serving.queue.cancels.lock().expect("cancel table").remove(&address);
            serving.queue.running.send_modify(|n| *n -= 1);
        });
    }
}

/// The job's next round: numbered from its log, launched from its latest
/// request, collected into its log.
async fn run_queued_round<R: Runner>(
    serving: &Serving<R>,
    address: &JobAddress,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let report = address.report()?;
    let (Some(remote_url), Some(base), Some(prompt), Some(provider)) = (
        report.remote_url,
        report.base,
        report.latest_prompt,
        report.provider,
    ) else {
        anyhow::bail!("the job has no recorded request to run");
    };
    let command_limit_secs = {
        let _repo = serving.repos.hold(&address.key).await;
        RepoConfig::from_ref(&address.key.repo_cache(&serving.daemon.root), &base.sha)
            .await?
            .command_limit_secs()
    };
    let paths = address.paths();
    let round = report.rounds + 1;
    let mut log = EventLog::open_append(paths.events())?;
    log.append(EventKind::RoundStarted { round })?;

    let spec = LaunchSpec::for_round::<R>(
        address.id,
        round,
        &remote_url,
        &base,
        &prompt,
        &provider,
        command_limit_secs,
    );
    match serving.daemon.runner.launch(&spec, serving.daemon.secrets(), &cancel).await {
        Ok(running) => collect(running, &mut log, &paths.log(), cancel).await.map(|_| ()),
        Err(e) => record_launch_failure(&mut log, &e).map(|_| ()),
    }
}
```

`TaskTracker::spawn` needs the tracker to be open; it is until
`stop_rounds_and_wait` closes it, after which the dispatcher has already
returned on `stopping`.

`src/daemon/submit.rs`:

```rust
//! A submit, as the daemon takes it: preflight in its own bare cache, then
//! claim and record. Nothing is claimed for a job that would be refused.

use super::Serving;
use super::api::{Queued, Refused, Submission};
use super::dispatch::JobAddress;
use crate::claim::claim_job;
use crate::config::{REPO_CONFIG_PATH, RepoConfig};
use crate::event::{EventKind, EventLog};
use crate::git;
use crate::job::JobId;
use crate::paths::{self, RepoKey};
use crate::report::JobReport;
use crate::runner::{Runner, reasons_a_container_cannot_run};
use crate::state::JobState;
use std::path::Path;

fn refused(summary: impl Into<String>, reasons: Vec<String>) -> Refused {
    Refused {
        summary: summary.into(),
        reasons,
    }
}

fn unpreparable(e: impl std::fmt::Display) -> Refused {
    refused(e.to_string(), Vec::new())
}

/// A job's own base and provider, which a revise starts from.
struct Continued {
    id: JobId,
    base_ref: String,
    provider: String,
}

/// Preflight `submission`, claim its job if it is new, record the request
/// and queue it.
///
/// # Errors
///
/// A [`Refused`] naming every reason the job cannot run, before anything is
/// claimed or recorded.
pub async fn accept<R: Runner>(
    serving: &Serving<R>,
    submission: Submission,
) -> Result<Queued, Refused> {
    let url = submission.remote_url.clone();
    let key = RepoKey::from_remote_url(&url).map_err(unpreparable)?;
    if R::RUNS_IN_A_CONTAINER {
        let problems = reasons_a_container_cannot_run(&url);
        if !problems.is_empty() {
            return Err(refused(
                "the job cannot run on this daemon's runner",
                problems.iter().map(ToString::to_string).collect(),
            ));
        }
    }
    let root = &serving.daemon.root;
    let jobs_dir = key.jobs_dir(root);
    let cache = key.repo_cache(root);
    let _repo = serving.repos.hold(&key).await;
    git::init_bare_if_absent(&cache).await.map_err(unpreparable)?;

    let continued = match submission.job {
        Some(id) => Some(continued_job(&jobs_dir, JobId::from(id), &cache, &url).await?),
        None => None,
    };
    let (base_ref, provider_named) = match &continued {
        Some(job) => (job.base_ref.clone(), Some(job.provider.clone())),
        None => (
            submission
                .base_ref
                .clone()
                .ok_or_else(|| unpreparable("a new job needs a ref to start from"))?,
            submission.provider.clone(),
        ),
    };
    let base = git::pinned(&cache, &url, &base_ref).await.map_err(unpreparable)?;
    let config = RepoConfig::from_ref(&cache, &base.sha).await.map_err(unpreparable)?;
    let provider = provider_named
        .or_else(|| config.provider.clone())
        .unwrap_or_default();
    let problems = config.reasons_it_cannot_run(&provider);
    if !problems.is_empty() {
        return Err(refused(
            format!("{REPO_CONFIG_PATH} is not runnable"),
            problems.iter().map(ToString::to_string).collect(),
        ));
    }

    let id = match continued {
        Some(job) => job.id,
        None => claim_job(&cache, &url, &base.sha).await.map_err(unpreparable)?,
    };
    let paths = paths::create_job(&jobs_dir, id).map_err(unpreparable)?;
    EventLog::open_append(paths.events())
        .and_then(|mut log| {
            log.append(EventKind::RoundRequested {
                remote_url: url.clone(),
                base,
                prompt: submission.prompt,
                provider,
            })
        })
        .map_err(unpreparable)?;
    serving.queue.enqueue(JobAddress { key, jobs_dir, id });

    Ok(Queued {
        job: id.into(),
        branch: id.branch_name(),
        warnings: config
            .settings_worth_flagging()
            .iter()
            .map(ToString::to_string)
            .collect(),
    })
}

/// The job a revise continues, once it is known to exist, to be idle, and to
/// have a branch to continue.
async fn continued_job(
    jobs_dir: &Path,
    id: JobId,
    cache: &Path,
    url: &str,
) -> Result<Continued, Refused> {
    let paths = paths::open_job(jobs_dir, id).map_err(unpreparable)?;
    let events = EventLog::read(paths.events()).map_err(unpreparable)?;
    let report = JobReport::from_events(id.into(), &events);
    if matches!(report.state, JobState::Queued | JobState::Running) {
        return Err(unpreparable(format!(
            "job {id} is already queued or running — wait for its verdict, or cancel it"
        )));
    }
    let (Some(base), Some(provider)) = (report.base, report.provider) else {
        return Err(unpreparable(format!("job {id} has no recorded request to continue")));
    };
    match git::remote_lacks_ref(cache, url, &id.branch_name()).await {
        Ok(true) => Err(unpreparable(format!(
            "job {id} has no branch on the remote — there is nothing to continue"
        ))),
        Ok(false) => Ok(Continued { id, base_ref: base.name, provider }),
        Err(e) => Err(unpreparable(e)),
    }
}
```

`src/daemon/mod.rs` — add `pub mod dispatch; pub mod submit;`, the shared
state, and serve it:

```rust
/// What every route and every round shares while the daemon runs.
#[derive(Debug)]
pub struct Serving<R> {
    pub daemon: Daemon<R>,
    pub repos: dispatch::RepoLocks,
    pub queue: dispatch::JobQueue,
}

pub async fn serve<R: Runner + Send + Sync + 'static>(
    daemon: Daemon<R>,
    lock: RootLock,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let socket = socket_path(&daemon.root);
    let listener = tokio::net::UnixListener::bind(&socket)?;
    let (queue, pending) = dispatch::JobQueue::new();
    let serving = Arc::new(Serving {
        daemon,
        repos: dispatch::RepoLocks::default(),
        queue,
    });
    let dispatcher = tokio::spawn(dispatch::dispatch_until_closed(Arc::clone(&serving), pending));

    let served = axum::serve(listener, api::router(Arc::clone(&serving)))
        .with_graceful_shutdown(shutdown)
        .await;
    // P7: until reattach, a round the daemon cannot come back to is
    // cancelled and given its verdict before the daemon goes.
    serving.queue.stop_rounds_and_wait().await;
    dispatcher.abort();
    let _ = std::fs::remove_file(&socket);
    drop(lock);
    served.map_err(Into::into)
}
```

- [ ] **Step 6: The CLI's side of a submit**

`src/submission.rs`:

```rust
//! `submit`, before it reaches the daemon: which remote, which ref, what
//! prompt — resolved from the checkout without writing anything to it.

use crate::daemon::api::Submission;
use crate::git;
use crate::payload::remote_to_clone;
use crate::run::prompt_text;
use crate::workspace::DEFAULT_REMOTE;
use anyhow::anyhow;
use std::path::{Path, PathBuf};

/// What `submit` was asked for, as given on the command line.
#[derive(Debug, Clone, Default)]
pub struct SubmitRequest {
    pub prompt: Option<String>,
    pub prompt_file: Option<PathBuf>,
    /// A checkout, or a remote URL — as `run` takes it.
    pub repo: Option<String>,
    pub base_ref: Option<String>,
    pub provider: Option<String>,
    pub job: Option<u64>,
}

/// A job starts from the remote's copy of a ref. When the user's own copy
/// differs — usually unpushed commits — they should hear so, rather than
/// wonder where their work went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalRefDiffers {
    pub base_ref: String,
    pub remote: String,
    pub remote_sha: String,
}

impl std::fmt::Display for LocalRefDiffers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "note: your '{}' is not what '{}' has — the job starts from {}'s ({}); push first \
             if you meant yours",
            self.base_ref,
            self.remote,
            self.remote,
            &self.remote_sha[..12.min(self.remote_sha.len())]
        )
    }
}

#[derive(Debug)]
pub struct PreparedSubmission {
    pub submission: Submission,
    pub notes: Vec<LocalRefDiffers>,
}

/// # Errors
///
/// When the prompt file cannot be read, there is no repository or remote,
/// or no ref can be settled on for a new job.
pub async fn prepare_submission(request: SubmitRequest) -> anyhow::Result<PreparedSubmission> {
    let prompt = prompt_text(request.prompt, request.prompt_file)?;
    let checkout = match request.repo {
        Some(named) if Path::new(&named).join(".git").exists() => PathBuf::from(named),
        Some(url) => {
            return for_remote_url(url, request.job, request.base_ref, prompt, request.provider);
        }
        None => {
            let cwd = std::env::current_dir()?;
            crate::paths::git_root(&cwd)
                .ok_or_else(|| anyhow!("not inside a git repository — name one with --repo"))?
        }
    };
    let remote_url = remote_to_clone(&checkout, DEFAULT_REMOTE).await?;
    let base_ref = match (request.job, request.base_ref) {
        (Some(_), _) => None,
        (None, Some(named)) => Some(named),
        (None, None) => Some(git::current_branch(&checkout).await?.ok_or_else(|| {
            anyhow!("HEAD is detached — name the ref to start from with --ref")
        })?),
    };
    let notes = match &base_ref {
        None => Vec::new(),
        Some(base_ref) => local_ref_differs(&checkout, base_ref).await.into_iter().collect(),
    };
    Ok(PreparedSubmission {
        submission: Submission {
            remote_url,
            base_ref,
            job: request.job,
            prompt,
            provider: request.provider,
        },
        notes,
    })
}

/// A submission naming a remote URL: there is no checkout to take a branch
/// or a note from, so a new job must name its ref.
fn for_remote_url(
    remote_url: String,
    job: Option<u64>,
    base_ref: Option<String>,
    prompt: String,
    provider: Option<String>,
) -> anyhow::Result<PreparedSubmission> {
    let base_ref = match (job, base_ref) {
        (Some(_), _) => None,
        (None, Some(named)) => Some(named),
        (None, None) => {
            return Err(anyhow!(
                "a remote URL has no checked-out branch — name the ref to start from with --ref"
            ));
        }
    };
    Ok(PreparedSubmission {
        submission: Submission { remote_url, base_ref, job, prompt, provider },
        notes: Vec::new(),
    })
}

async fn local_ref_differs(checkout: &Path, base_ref: &str) -> Option<LocalRefDiffers> {
    let local = git::sha_at_ref(checkout, base_ref).await.ok()?;
    let remote = git::sha_on_remote(checkout, DEFAULT_REMOTE, base_ref).await.ok()??;
    (local != remote).then(|| LocalRefDiffers {
        base_ref: base_ref.to_string(),
        remote: DEFAULT_REMOTE.to_string(),
        remote_sha: remote,
    })
}
```

`src/cli.rs`: `Submit` loses `runner`, and its `repo` becomes
`Option<String>` with `run`'s help ("A checkout, or a remote URL. Defaults
to the enclosing checkout."). Its help: "Hand a job to the daemon:
a new job, or with --job, another round of an existing one".

`src/main.rs` — the `Submit` arm builds a `SubmitRequest` and calls:

```rust
/// `submit`: resolve, post to the daemon, report what it said.
async fn submit_to_daemon(root: &Path, request: SubmitRequest) -> Result<ExitCode, String> {
    let prepared = submission::prepare_submission(request)
        .await
        .map_err(|e| e.to_string())?;
    prepared.notes.iter().for_each(|note| println!("{note}"));
    match DaemonClient::for_root(root)
        .post_json::<_, Queued>("/jobs", &prepared.submission)
        .await
        .map_err(|e| e.to_string())?
    {
        Reply::Accepted(queued) => {
            queued.warnings.iter().for_each(|warning| eprintln!("warn: {warning}"));
            println!("{queued}");
            Ok(ExitCode::SUCCESS)
        }
        Reply::Refused(refused) => {
            refused.reasons.iter().for_each(|reason| eprintln!("error: {reason}"));
            Err(refused.summary)
        }
    }
}
```

Delete `run_work_on_chosen_runner`, `run_work`, `report_refusal`,
`cancel_on_ctrl_c` and the `lifecycle` imports. `status` and `logs` call
`locate::report_for_job` and `locate::output_log_of`.

`src/locate.rs` — move `report_for_job` and `output_log_of` in from
`lifecycle` (their bodies already use `job_at`).

Delete `src/lifecycle.rs` and `pub mod lifecycle;`; add `pub mod
submission;`. Delete `tests/lifecycle.rs` (its tests moved above; the two
about config notes and announcements test nothing that exists now).

- [ ] **Step 7: Move the CLI tests onto `run` and the daemon**

`submit` no longer runs a job in the foreground, so a CLI test that ran one
to see what a job does runs it with `run`, which does exactly that:

- `tests/cli.rs`: in `run_exits_zero_and_records_the_job` (rename
  `run_exits_zero_and_leaves_the_jobs_branch`, and assert the branch instead
  of the job directory), `run_exits_one_when_the_agent_fails_but_still_leaves_the_branch`,
  `a_job_id_already_taken_on_the_remote_is_skipped` (drop the job-directory
  assertion), `a_repository_that_has_not_opted_in_is_told_which_file_to_write`
  (assert no `al/job-*` on the origin instead of no directory),
  `a_repository_with_no_remote_is_told_to_add_one`,
  `a_job_whose_round_changed_nothing_can_still_be_revised`
  (`run --job 1 --prompt …`), `an_undeclared_provider_is_rejected_before_a_job_directory_is_allocated`
  (rename `…before_anything_is_claimed`, assert no `al/job-*`),
  `run_outside_a_git_repo_explains_itself`,
  `a_run_with_no_prompt_at_all_is_a_usage_error`, `a_prompt_can_come_from_a_file_instead`,
  `a_missing_prompt_file_is_reported_before_the_job_starts`,
  `a_global_push_rewrite_is_followed_rather_than_refused`,
  `a_job_branches_from_the_ref_it_is_given`, `a_detached_head_is_asked_to_name_its_ref`,
  `a_revise_round_continues_the_branch_instead_of_starting_over`
  (`run --job 1 --prompt …`) and
  `a_job_writes_nothing_to_the_target_repositorys_working_tree` (drop its
  `job_dir` assertion: `run` keeps no state), the verb `submit` becomes
  `run` and `--job N --prompt P` stays as it is. Delete `job_dir` and
  `RepoKey`'s import from `tests/cli.rs` once nothing uses them — an unused
  test helper fails `clippy -D warnings`.
- Delete from `tests/cli.rs` what moved to `tests/daemon_jobs.rs` above:
  `status_and_logs_report_a_finished_job` (covered by
  `a_submitted_job_is_queued_then_run_by_the_daemon`),
  `the_local_runner_keeps_the_hosts_credentials_even_with_a_token_exported`,
  `a_job_started_elsewhere_is_found_by_pointing_the_read_commands_at_it`,
  `unpushed_local_work_is_pointed_out_and_the_remotes_ref_is_used`,
  `docker_preflight_reports_every_problem_before_allocating` (Task 8's
  `a_daemon_whose_runner_cannot_run_does_not_start`) and
  `a_remote_that_is_a_local_path_is_refused_for_a_container_runner`.
- The runner-flag tests parse `daemon` now:
  `container_flags_are_refused_for_the_local_runner` runs
  `assembly daemon --pass-env ANTHROPIC_API_KEY` (with `ASSEMBLY_ROOT` set)
  and expects code 2 and "container runners";
  `the_k8s_runner_requires_a_namespace` runs `assembly daemon --runner k8s`;
  `a_namespace_is_refused_for_runners_that_have_none` runs
  `assembly daemon --runner docker --namespace factory`;
  `inapplicable_flags_of` matches `Subcommand::Daemon { runner, .. }`, and
  its three `#[test]`s parse `["assembly", "daemon", …]` without `--prompt`.
- `a_revise_without_a_prompt_is_a_usage_error` and
  `a_revise_cannot_name_a_ref_or_a_provider` stay on `submit` — clap
  refuses before any daemon is needed. `status_with_no_jobs_explains_itself`
  and `logs_for_an_unknown_job_explains_itself` stay as they are.
- `tests/delivery.rs`: its four `submit` tests switch to `run` (delivery is
  `run`'s): `configured_base_reaches_the_pull_request_and_the_divergence_is_reported`
  asserts the `note:` on `run`'s stdout again.

- [ ] **Step 8: Run the gate**

Run: `just check`
Expected: PASS. The daemon tests start real daemons; run the gate twice to
catch an ordering that only fails sometimes.

- [ ] **Step 9: Commit**

```bash
jj describe -m "feat(daemon)!: submit queues a job with the daemon, preflighted and claimed

submit resolves the checkout's remote and ref and posts the job; the
daemon pins the base in its own bare cache, validates the config at
that commit, checks the remote against its runner, claims al/job-N,
records the request and queues it. A dispatcher launches rounds oldest
first under --max-jobs and collects them. Nothing is fetched into the
user's .git any more. lifecycle is gone: its host half is the daemon's,
its reading half locate's. Stopping the daemon cancels its rounds and
waits for their verdicts.

Tests: <count>."
jj new
```

---

### Task 11: `cancel` stops a queued or running job

`assembly cancel N` asks the daemon to stop job N. A running round's cancel
token fires, and the round ends the way Ctrl-C ended one before: the runner
signals `run`, `run` stops its agent, keeps the work, and reports
`RoundFailed { reason: "cancelled" }`. A queued job never starts: the daemon
records `RoundFailed { reason: "cancelled before it started" }`, and the
dispatcher, which re-reads a job's state before launching it, skips it. Both
decisions are made under the queue's cancel-table lock, which the dispatcher
also holds while it checks and registers a round, so a job cannot be both
cancelled-while-queued and launched.

**Files:**
- Modify: `src/daemon/api.rs`, `src/daemon/dispatch.rs`, `src/cli.rs`, `src/main.rs`, `src/locate.rs`
- Test: `tests/daemon_jobs.rs`

**Interfaces:**
- Consumes: `dispatch::{JobAddress, JobQueue}`, `locate::jobs_dir_of`.
- Produces:
  - `daemon::api::CancelRequest { remote_url: String, job: u64 }`; `daemon::api::Cancelling { job: u64, was: JobState }` with `Display`
  - `POST /cancel` → `200 Cancelling` or `422 Refused`
  - `dispatch::JobQueue::cancel(&self, address: &JobAddress) -> Result<JobState, String>` (the state it was in)
  - `cli::Command::Cancel { job_id: u64, repo: Option<PathBuf> }`

- [ ] **Step 1: Write the failing tests**

`tests/daemon_jobs.rs`:

```rust
#[tokio::test]
async fn cancelling_a_running_job_stops_its_agent_and_records_it() {
    let fx = Fixture::running("sleeping-agent.sh", &[], &[]).await;
    fx.assembly().args(["submit", "--prompt", "x"]).assert().success();
    wait_until_running(&fx.job_dir(1));

    fx.assembly()
        .args(["cancel", "1"])
        .assert()
        .success()
        .stdout(contains("job 1: cancelling"));

    let report = wait_for_verdict(&fx.job_dir(1));
    assert_eq!(report.state, JobState::Failed);
    assert_eq!(report.detail.as_deref(), Some("cancelled"));
    let scratch = fx.tmp.path().join("scratch");
    assert!(
        std::fs::read_dir(&scratch).map_or(true, |mut entries| entries.next().is_none()),
        "the cancelled round left its clone behind"
    );
}

#[tokio::test]
async fn cancelling_a_queued_job_means_it_never_runs() {
    let fx = Fixture::running("sleeping-agent.sh", &["--max-jobs", "1"], &[]).await;
    fx.assembly().args(["submit", "--prompt", "first"]).assert().success();
    fx.assembly().args(["submit", "--prompt", "second"]).assert().success();
    wait_until_running(&fx.job_dir(1));

    fx.assembly()
        .args(["cancel", "2"])
        .assert()
        .success()
        .stdout(contains("job 2: cancelled before it started"));
    fx.assembly().args(["cancel", "1"]).assert().success();

    assert_eq!(wait_for_verdict(&fx.job_dir(1)).state, JobState::Failed);
    let second = wait_for_verdict(&fx.job_dir(2));
    assert_eq!(second.rounds, 0, "the cancelled job was started anyway");
    assert_eq!(second.detail.as_deref(), Some("cancelled before it started"));
}

#[tokio::test]
async fn cancelling_a_job_that_is_not_running_is_refused() {
    let fx = Fixture::running("fake-agent.sh", &[], &[]).await;
    fx.assembly().args(["submit", "--prompt", "x"]).assert().success();
    wait_for_verdict(&fx.job_dir(1));

    fx.assembly()
        .args(["cancel", "1"])
        .assert()
        .code(2)
        .stderr(contains("job 1 is not queued or running"));
}

#[tokio::test]
async fn cancel_with_no_daemon_says_how_to_start_one() {
    let fx = Fixture::repository(&support::config_running("fake-agent.sh")).await;

    fx.assembly()
        .args(["cancel", "1"])
        .assert()
        .code(2)
        .stderr(contains("assembly daemon"));
}
```

`a_revise_of_a_job_still_running_is_refused` ends with
`fx.assembly().args(["cancel", "1"]).assert().success();`, so its sleeping
agent does not outlive the test.

- [ ] **Step 2: Run them to make sure they fail**

Run: `cargo test --test daemon_jobs cancel`
Expected: FAIL — `unrecognized subcommand 'cancel'`.

- [ ] **Step 3: Implement**

`src/daemon/dispatch.rs` — `JobQueue::cancel`, and the dispatcher's check
and registration under the same lock:

```rust
    /// Stop `address`'s job: fire a running round's cancel, or close a
    /// queued job before it starts. Returns the state it was in.
    ///
    /// # Errors
    ///
    /// Why it cannot be cancelled: it is neither queued nor running, or its
    /// log cannot be read or written.
    ///
    /// # Panics
    ///
    /// If a holder of the cancel table panicked.
    pub fn cancel(&self, address: &JobAddress) -> Result<JobState, String> {
        let cancels = self.cancels.lock().expect("cancel table");
        if let Some(running) = cancels.get(address) {
            running.cancel();
            return Ok(JobState::Running);
        }
        let state = address.report().map_err(|e| e.to_string())?.state;
        match state {
            JobState::Queued => EventLog::open_append(address.paths().events())
                .and_then(|mut log| {
                    log.append(EventKind::RoundFailed {
                        reason: "cancelled before it started".to_string(),
                    })
                })
                .map(|_| JobState::Queued)
                .map_err(|e| e.to_string()),
            _ => Err(format!("job {} is not queued or running", address.id)),
        }
    }
```

In `dispatch_until_closed`, replace the "still queued" check and the cancel
registration with one locked step:

```rust
        let cancel = {
            let mut cancels = serving.queue.cancels.lock().expect("cancel table");
            // A job cancelled while it waited is no longer queued.
            match address.report().map(|r| r.state) {
                Ok(JobState::Queued) => {
                    let cancel = serving.queue.stopping.child_token();
                    cancels.insert(address.clone(), cancel.clone());
                    cancel
                }
                _ => continue,
            }
        };
```

`src/daemon/api.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelRequest {
    pub remote_url: String,
    pub job: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cancelling {
    pub job: u64,
    /// What the job was doing when it was asked to stop.
    pub was: JobState,
}

impl std::fmt::Display for Cancelling {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.was {
            JobState::Queued => write!(f, "job {}: cancelled before it started", self.job),
            _ => write!(f, "job {}: cancelling — its verdict follows in `assembly status`", self.job),
        }
    }
}
```

`JobState` derives `Serialize, Deserialize` with `rename_all = "snake_case"`
for this. The route:

```rust
async fn cancel_job<R: Runner + Send + Sync + 'static>(
    State(serving): State<Arc<Serving<R>>>,
    Json(request): Json<CancelRequest>,
) -> Result<Json<Cancelling>, (StatusCode, Json<Refused>)> {
    let refuse = |summary: String| {
        (StatusCode::UNPROCESSABLE_ENTITY, Json(Refused { summary, reasons: Vec::new() }))
    };
    let key = RepoKey::from_remote_url(&request.remote_url).map_err(|e| refuse(e.to_string()))?;
    let address = JobAddress {
        jobs_dir: key.jobs_dir(&serving.daemon.root),
        key,
        id: JobId::from(request.job),
    };
    serving
        .queue
        .cancel(&address)
        .map(|was| Json(Cancelling { job: request.job, was }))
        .map_err(refuse)
}
```

registered as `.route("/cancel", post(cancel_job::<R>))`.

`src/cli.rs`:

```rust
    /// Stop a queued or running job. A running round keeps what its agent
    /// did so far, on the job's branch.
    Cancel {
        job_id: u64,
        /// The repository the job belongs to. Defaults to the enclosing one.
        #[arg(long)]
        repo: Option<PathBuf>,
    },
```

`src/main.rs`: the arm runs in the async runtime, gets the remote URL with
`locate::jobs_dir_of(&root, repo)`, posts a `CancelRequest` to `/cancel`, and
prints the `Cancelling` or the refusal's summary as a usage error — the same
shape as `submit_to_daemon`.

- [ ] **Step 4: Run the gate**

Run: `just check`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
jj describe -m "feat(daemon): cancel stops a queued or running job

A running round is cancelled the way Ctrl-C cancelled one: run stops
its agent, keeps the work, and reports the round. A queued job is closed
before it starts. The dispatcher checks and registers a round under the
same lock cancel decides under, so a job is never both.

Tests: <count>."
jj new
```

---

### Task 12: A restarted daemon reattaches to the rounds it left running

Stopping the daemon stops no job (the spec's *Stopping the daemon stops no
job*): its rounds finish their arc — pull request included — without it,
and the next daemon on the root picks each one up where its log left off.
Four pieces make that true:

1. **Every round records how to find it again.** After a successful launch
   the daemon appends `RoundLaunched { handle }` — the local round's pid and
   frames file, the docker container's name, or the k8s Job's name.
2. **Every runner can resume a stream.** The local runner starts `run` in a
   session of its own with stdout and stderr on a per-round `frames` file,
   and reads that file; it no longer holds `run` by a pipe, so `run`
   outlives the daemon. Docker creates the container, then starts it, then
   follows `docker logs -f` — so a cancel always has a container to stop
   (Accepted risk 12, closed), and a reattach is another `docker logs -f`.
   k8s already follows `kubectl logs -f` and finds its pod by Job name.
3. **The collector resumes after a seq.** It rewrites a per-round `position`
   file with the last seq it routed (P6), and a resumed collection starts
   after it, so a replayed stream adds nothing twice.
4. **The daemon folds its root on start.** A round launched without a
   verdict is reattached; a round started but never launched — the daemon
   died mid-launch — is closed as failed, saying so; a queued job goes back
   in the queue, oldest request first.

P7 goes: stopping the daemon no longer cancels anything.

**Files:**
- Create: `src/runner/tail.rs`, `src/daemon/fleet.rs`, `tests/fleet.rs`, `tests/fixtures/gated-agent.sh`
- Modify: `src/runner/mod.rs`, `src/runner/local.rs`, `src/runner/docker.rs`, `src/runner/kubernetes.rs`, `src/runner/child.rs`, `src/event.rs`, `src/report.rs`, `src/frame.rs`, `src/collect.rs`, `src/daemon/mod.rs`, `src/daemon/dispatch.rs`, `src/paths.rs`
- Test: `tests/collect.rs`, `tests/docker_runner.rs`, `tests/kubernetes_runner.rs`, `tests/runner.rs`, `tests/daemon_jobs.rs`, `tests/support/mod.rs`

**Interfaces:**
- Consumes: `dispatch::{JobAddress, JobQueue, run_queued_round}`, `paths::existing_job_ids`, `exec::detach_from_terminal`.
- Produces:
  - `runner::RoundHandle { Local { pid: i32, frames: PathBuf }, Docker { container: String }, Kubernetes { job: String } }` (serde, tagged `runner`)
  - `Runner::reattach(&self, handle: &RoundHandle) -> impl Future<Output = anyhow::Result<Self::Running>> + Send`
  - `RunningRound::handle(&self) -> RoundHandle`
  - `LaunchSpec::frames_file: PathBuf` — `for_round` takes it as its last argument
  - `runner::tail::FileTail` — `open(path: &Path) -> io::Result<FileTail>`, `next_line(&mut self, still_writing: impl Fn() -> bool) -> Option<String>` (async)
  - `event::EventKind::RoundLaunched { handle: RoundHandle }`; `JobReport::launched: Option<RoundHandle>`; `JobReport::requested_at: Option<DateTime<Utc>>`
  - `frame::StreamPosition::after(last_seq: u64) -> StreamPosition`, `StreamPosition::last_seq(self) -> u64`
  - `collect::collect(running, log, output_log, position_file: &Path, cancel)`
  - `JobPaths::frames(round: u32) -> PathBuf`, `JobPaths::position(round: u32) -> PathBuf`
  - `daemon::fleet::Resumption { Requeue, Reattach { round: u32, handle: RoundHandle }, LostWhileLaunching { round: u32 } }`,
    `Resumption::for_report(report: &JobReport) -> Option<Resumption>`,
    `fleet::jobs_under(root: &Path) -> anyhow::Result<Vec<(JobAddress, JobReport)>>`
- Deleted: `JobQueue::stop_rounds_and_wait`, the `stopping` token's use at shutdown.

- [ ] **Step 1: Write the failing unit tests**

`tests/fleet.rs`:

```rust
//! What a daemon starting on a root owes each job it finds there.

use assembly_line::daemon::fleet::Resumption;
use assembly_line::event::{Event, EventKind};
use assembly_line::git::PinnedRef;
use assembly_line::report::JobReport;
use assembly_line::runner::RoundHandle;

fn report_of(kinds: Vec<EventKind>) -> JobReport {
    let events: Vec<Event> = kinds
        .into_iter()
        .map(|kind| Event { at: chrono::Utc::now(), kind })
        .collect();
    JobReport::from_events(1, &events)
}

fn requested() -> EventKind {
    EventKind::RoundRequested {
        remote_url: "/o.git".into(),
        base: PinnedRef { name: "main".into(), sha: "a".repeat(40) },
        prompt: "x".into(),
        provider: "p".into(),
    }
}

fn container() -> RoundHandle {
    RoundHandle::Docker { container: "al-1-1-x".into() }
}

#[test]
fn a_job_still_queued_goes_back_in_the_queue() {
    assert_eq!(Resumption::for_report(&report_of(vec![requested()])), Some(Resumption::Requeue));
}

#[test]
fn a_round_launched_without_a_verdict_is_reattached() {
    let report = report_of(vec![
        requested(),
        EventKind::RoundStarted { round: 1 },
        EventKind::RoundLaunched { handle: container() },
    ]);

    assert_eq!(
        Resumption::for_report(&report),
        Some(Resumption::Reattach { round: 1, handle: container() })
    );
}

#[test]
fn a_round_started_but_never_launched_was_lost_while_launching() {
    let report = report_of(vec![requested(), EventKind::RoundStarted { round: 2 }]);

    assert_eq!(Resumption::for_report(&report), Some(Resumption::LostWhileLaunching { round: 2 }));
}

#[test]
fn a_job_with_a_verdict_is_owed_nothing() {
    let report = report_of(vec![
        requested(),
        EventKind::RoundStarted { round: 1 },
        EventKind::RoundLaunched { handle: container() },
        EventKind::RoundPassed,
    ]);

    assert_eq!(Resumption::for_report(&report), None);
}

#[test]
fn a_handle_round_trips_through_the_event_log() {
    let handle = RoundHandle::Local { pid: 42, frames: "/r/round-1.frames".into() };
    let line = serde_json::to_string(&EventKind::RoundLaunched { handle: handle.clone() }).unwrap();

    assert!(line.contains("\"runner\":\"local\""), "{line}");
    assert_eq!(
        serde_json::from_str::<EventKind>(&line).unwrap(),
        EventKind::RoundLaunched { handle }
    );
}
```

`tests/collect.rs` — a stream replayed from the start after a reconnect adds
nothing twice:

```rust
/// A round's stream as a fixed list of lines, for feeding the collector.
struct Replayed {
    lines: std::collections::VecDeque<String>,
}

impl RunningRound for Replayed {
    async fn next_line(&mut self) -> Option<String> {
        self.lines.pop_front()
    }
    async fn cancel(&mut self) {}
    async fn termination(self) -> Termination {
        Termination::Exited(0)
    }
    fn handle(&self) -> RoundHandle {
        RoundHandle::Docker { container: "replayed".into() }
    }
}

fn frame_lines(bodies: &[FrameBody]) -> Vec<String> {
    bodies
        .iter()
        .zip(1..)
        .map(|(body, seq)| serde_json::to_string(&Frame { seq, body: body.clone() }).unwrap())
        .collect()
}

#[tokio::test]
async fn a_resumed_collection_skips_what_it_already_has() {
    let dir = tempfile::tempdir().unwrap();
    let (events, output, position) =
        (dir.path().join("events.jsonl"), dir.path().join("job.log"), dir.path().join("position"));
    let passed = Event { at: chrono::Utc::now(), kind: EventKind::RoundPassed };
    let stream = frame_lines(&[
        FrameBody::Output("one".into()),
        FrameBody::Output("two".into()),
        FrameBody::Output("three".into()),
        FrameBody::Event(passed),
    ]);
    let mut log = EventLog::open_append(&events).unwrap();
    let first_two = Replayed { lines: stream[..2].iter().cloned().collect() };
    collect(first_two, &mut log, &output, &position, CancellationToken::new()).await.unwrap();

    let everything_again = Replayed { lines: stream.iter().cloned().collect() };
    let verdict = collect(everything_again, &mut log, &output, &position, CancellationToken::new())
        .await
        .unwrap();

    assert!(verdict.passed());
    assert_eq!(std::fs::read_to_string(&output).unwrap(), "one\ntwo\nthree\n");
    assert_eq!(EventLog::read(&events).unwrap().len(), 2, "one verdict from the first, one from the second");
}
```

The first collection ended without a verdict, so it recorded one of its own
(`verdict_missing_from`); the second recorded the real one. That is two
events, and the assertion says why. If you would rather the first stream
not end — which is what a daemon stopping mid-round looks like — cancel it
instead; either way the point is the output log has no line twice.

- [ ] **Step 2: Run them to make sure they fail**

Run: `cargo test --test fleet --test collect`
Expected: FAIL to compile — `no RoundHandle in runner`, `no fleet in daemon`.

- [ ] **Step 3: Handles, the event, and the fold**

`src/runner/mod.rs`:

```rust
/// Enough to find a launched round again — from another daemon process, if
/// need be. Recorded in the job's log as it launches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "runner", rename_all = "snake_case")]
pub enum RoundHandle {
    /// `run` leading a session of its own, writing its frames to `frames`.
    Local { pid: i32, frames: PathBuf },
    Docker { container: String },
    #[serde(rename = "k8s")]
    Kubernetes { job: String },
}
```

`Runner` gains:

```rust
    /// Pick up a round this runner launched earlier — possibly from a
    /// daemon process that has since gone — by the handle it recorded.
    fn reattach(
        &self,
        handle: &RoundHandle,
    ) -> impl Future<Output = anyhow::Result<Self::Running>> + Send;
```

and `RunningRound` gains `fn handle(&self) -> RoundHandle;`. A handle of
another runner's kind is an error naming both:
`anyhow!("the round was launched by a {} runner, and this daemon runs {}", …)`.

`LaunchSpec` gains `pub frames_file: PathBuf` ("where a local round writes
its frames; container runners ignore it"), and `for_round` takes it last.
`tests/support/mod.rs`'s `launch_spec_for` passes
`self.scratch_root().with_file_name("round-1.frames")`; `tests/runner.rs`
passes any path.

`src/event.rs`, after `RoundStarted`:

```rust
    /// The round is running on its runner, and this is how to find it again.
    RoundLaunched {
        handle: crate::runner::RoundHandle,
    },
```

`src/report.rs`: `launched: Option<RoundHandle>` — set by `RoundLaunched`,
cleared by `RoundStarted` and by `RoundRequested` — and
`requested_at: Option<DateTime<Utc>>`, the time of the latest
`RoundRequested`. Neither appears in `to_status_lines`.

`src/paths.rs`:

```rust
    /// Where a local round `round` writes its frames.
    #[must_use]
    pub fn frames(&self, round: u32) -> PathBuf {
        self.dir.join(format!("round-{round}.frames"))
    }

    /// The last seq the collector routed for round `round`.
    #[must_use]
    pub fn position(&self, round: u32) -> PathBuf {
        self.dir.join(format!("round-{round}.position"))
    }
```

- [ ] **Step 4: The collector resumes**

`src/frame.rs`:

```rust
impl StreamPosition {
    /// A position past `last_seq`, for a collection resuming a stream.
    #[must_use]
    pub fn after(last_seq: u64) -> StreamPosition {
        StreamPosition { last_seq }
    }

    #[must_use]
    pub fn last_seq(self) -> u64 {
        self.last_seq
    }
    …
}
```

`src/collect.rs`: `collect(running, log, output_log, position_file, cancel)`.
`stream_into_logs` starts from
`StreamPosition::after(read_position(position_file))` and, after writing
each routed `Event` or `Output`, rewrites the position file with
`position.last_seq().to_string()`:

```rust
/// The last seq a collection of this round routed, or 0 for a first one.
fn read_position(position_file: &Path) -> u64 {
    std::fs::read_to_string(position_file)
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0)
}
```

(`std::fs::write` truncates and rewrites: this file is the collector's
bookmark, not the event log, and the append-only rule does not cover it.)

- [ ] **Step 5: The local runner writes to a file and reads it back**

`src/runner/tail.rs`:

```rust
//! Reading a file another process is still appending to, line by line.

use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};

/// How long to wait before looking again at a file that has nothing new.
const POLL: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub struct FileTail {
    reader: BufReader<tokio::fs::File>,
    partial: String,
}

impl FileTail {
    /// # Errors
    ///
    /// When the file cannot be opened.
    pub async fn open(path: &Path) -> std::io::Result<FileTail> {
        Ok(FileTail {
            reader: BufReader::new(tokio::fs::File::open(path).await?),
            partial: String::new(),
        })
    }

    /// The next complete line, waiting for more while `still_writing` says
    /// the writer is alive; `None` once it is gone and the file is drained.
    /// A last line with no newline is returned as it is.
    ///
    /// A loop: each read either finds a line or waits and reads again.
    pub async fn next_line(&mut self, mut still_writing: impl FnMut() -> bool) -> Option<String> {
        loop {
            let writer_alive = still_writing();
            match self.reader.read_line(&mut self.partial).await {
                Ok(0) | Err(_) if !writer_alive => {
                    return (!self.partial.is_empty()).then(|| std::mem::take(&mut self.partial));
                }
                Ok(_) if self.partial.ends_with('\n') => {
                    let line = std::mem::take(&mut self.partial);
                    return Some(line.trim_end_matches('\n').to_string());
                }
                Ok(_) | Err(_) => tokio::time::sleep(POLL).await,
            }
        }
    }
}
```

`still_writing` is sampled *before* the read, so a writer that finishes
between a read and the check cannot strand its last lines: the next pass
reads with `writer_alive == false` and drains.

`src/runner/local.rs`:

```rust
impl LocalRunner {
    fn spawn_run(&self, spec: &LaunchSpec) -> anyhow::Result<LocalRound> {
        if let Some(dir) = spec.frames_file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let frames = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&spec.frames_file)?;
        let mut command = Command::new(&self.program);
        crate::exec::detach_from_terminal(&mut command)
            .args(&spec.args)
            .env_remove(GIT_TOKEN_VAR)
            .stdin(Stdio::null())
            .stdout(frames.try_clone()?)
            .stderr(frames);
        // Not `kill_on_drop`: the round must outlive this process.
        let child = command.spawn()?;
        let pid = child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .ok_or_else(|| anyhow::anyhow!("the round exited before it could be watched"))?;
        Ok(LocalRound {
            pid,
            frames: spec.frames_file.clone(),
            tail: None,
            ours: Some(child),
        })
    }
}

/// `run` leading its own session, read back from its frames file. `ours`
/// is the child when this process launched it; a reattached round was
/// launched by an earlier daemon and is known only by its pid.
#[derive(Debug)]
pub struct LocalRound {
    pid: i32,
    frames: PathBuf,
    tail: Option<FileTail>,
    ours: Option<tokio::process::Child>,
}

impl LocalRound {
    /// Whether `run` is still running. A child of ours is asked directly,
    /// which also reaps it; anyone else's pid is probed with signal 0.
    fn still_running(&mut self) -> bool {
        match &mut self.ours {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => !matches!(
                nix::sys::signal::kill(Pid::from_raw(self.pid), None),
                Err(nix::errno::Errno::ESRCH)
            ),
        }
    }
}

impl RunningRound for LocalRound {
    async fn next_line(&mut self) -> Option<String> {
        if self.tail.is_none() {
            self.tail = FileTail::open(&self.frames).await.ok();
        }
        // Taken out of `self` for the read, so the liveness probe can
        // borrow `self` mutably beside it.
        let mut tail = self.tail.take()?;
        let line = tail.next_line(|| self.still_running()).await;
        self.tail = Some(tail);
        line
    }

    /// SIGTERM to the round's whole session, which `run` leads: `run`
    /// answers by cancelling its agent and reporting the round.
    async fn cancel(&mut self) {
        let _ = nix::sys::signal::killpg(Pid::from_raw(self.pid), Signal::SIGTERM);
    }

    async fn termination(self) -> Termination {
        match self.ours {
            Some(mut child) => Termination::Exited(
                child.wait().await.ok().and_then(|s| s.code()).unwrap_or(-1),
            ),
            None => Termination::Killed {
                reason: "it ran while the daemon was down, so its exit status is unknown".into(),
            },
        }
    }

    fn handle(&self) -> RoundHandle {
        RoundHandle::Local { pid: self.pid, frames: self.frames.clone() }
    }
}
```

`Runner for LocalRunner`: `launch` returns `std::future::ready(self.spawn_run(spec))`;
`reattach` accepts only `RoundHandle::Local { pid, frames }` and returns a
`LocalRound { pid, frames, tail: None, ours: None }`. Because the child is
not ours after a restart, a pid reused by an unrelated process would look
alive until that process exits; the round's own frames would already have
carried its verdict, which the collector records either way.

- [ ] **Step 6: Docker creates, starts, follows, waits**

`src/runner/docker.rs`:

- `docker_run_args` becomes `docker_create_args` — the same list with
  `create` in place of `run`.
- `launch` runs `docker create` as a child it can abandon: on `cancel`,
  kill that client and `docker rm -f <name>` (the create may have got as far
  as making the container), and return
  `Err(anyhow!("cancelled before the container started"))`; otherwise
  `docker start <name>` (checked for success), then
  `DockerRound::following(program, name)`.
- `DockerRound::following` spawns `docker logs -f <name>` as its
  `ChildLines`. `reattach(RoundHandle::Docker { container })` is the same
  call. `handle()` is `RoundHandle::Docker { container }`.
- `termination` reaps any `docker stop` it started, then asks
  `docker wait <name>` for the exit code (its stdout is the code), checks
  OOM as before for a non-zero code, and removes the container.
- `cancel`'s doc loses the "a cancel that arrives before the container
  exists … is lost" paragraph: `launch` returns only once the container
  exists.

`tests/docker_runner.rs` — the fake `docker` learns the four verbs, keeping
each container's state in files under its own directory:

```rust
fn fake_docker(dir: &Path, argv_log: &Path, oom: bool) -> PathBuf {
    fake_cli(
        dir,
        "docker",
        &format!(
            "echo \"$*\" >> {log}\n\
             state={state}\n\
             mkdir -p \"$state\"\n\
             case \"$1\" in\n\
               create) shift; name=\"\"\n\
                 while [ \"$1\" != assembly ]; do [ \"$1\" = --name ] && name=$2; shift; done; shift\n\
                 printf '%s\\n' \"$@\" > \"$state/$name.args\"; echo \"$name\" ;;\n\
               start) name=$2\n\
                 ( mapfile -t args < \"$state/$name.args\"\n\
                   {bin} \"${{args[@]}}\" > \"$state/$name.log\" 2>&1; echo $? > \"$state/$name.exit\" ) &\n\
                 echo $! > \"$state/$name.pid\" ;;\n\
               logs) name=$3\n\
                 while [ ! -e \"$state/$name.exit\" ]; do sleep 0.05; done; cat \"$state/$name.log\" ;;\n\
               wait) name=$2\n\
                 while [ ! -e \"$state/$name.exit\" ]; do sleep 0.05; done; cat \"$state/$name.exit\" ;;\n\
               stop) pkill -TERM -P \"$(cat \"$state/$4.pid\")\" || true ;;\n\
               version) echo 27.0.0 ;;\n\
               inspect) echo {oom} ;;\n\
               *) ;;\n\
             esac\n",
            log = argv_log.display(),
            state = dir.join("containers").display(),
            bin = env!("CARGO_BIN_EXE_assembly"),
        ),
    )
}
```

(`logs -f` here prints the whole log once the round ends rather than
streaming it; the collector cannot tell, and reattach replays it the same
way. `stop -t 30 <name>` puts the name in `$4`. The argument file holds one
argument per line, so a prompt with a newline in it is not a thing these
tests use.)

Add:

```rust
#[tokio::test]
async fn a_docker_round_is_reattached_by_its_container_name() {
    let h = Harness::new().await;
    let fakes = h.scratch_root().with_file_name("fakes");
    let argv = fakes.join("argv");
    let docker = DockerRunner { program: fake_docker(&fakes, &argv, false), image: "img:1".into() };
    let spec = h.launch_spec_for("x").await;
    let first = docker.launch(&spec, &both_tokens(), &CancellationToken::new()).await.unwrap();
    let handle = first.handle();
    drop(first);

    let again = docker.reattach(&handle).await.unwrap();
    let paths = h.job_paths();
    let mut log = EventLog::open_append(paths.events()).unwrap();
    let verdict = collect(again, &mut log, &paths.log(), &paths.position(1), CancellationToken::new())
        .await
        .unwrap();

    assert!(verdict.passed());
    assert!(std::fs::read_to_string(&argv).unwrap().lines().filter(|l| l.starts_with("create")).count() == 1);
}

/// Accepted risk 12, closed: a cancel during the image pull finds the
/// container the create made, or makes sure none is left.
#[tokio::test]
async fn a_round_cancelled_while_its_container_is_being_created_leaves_none() {
    let h = Harness::new().await;
    let fakes = h.scratch_root().with_file_name("fakes");
    let argv = fakes.join("argv");
    let docker = DockerRunner {
        program: fake_cli(&fakes, "docker", &format!(
            "echo \"$*\" >> {}\n[ \"$1\" = create ] && sleep 30\nexit 0\n",
            argv.display()
        )),
        image: "img:1".into(),
    };
    let spec = h.launch_spec_for("x").await;
    let cancel = CancellationToken::new();
    let cancel_soon = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        cancel_soon.cancel();
    });

    let started = std::time::Instant::now();
    let launched = docker.launch(&spec, &both_tokens(), &cancel).await;

    assert!(launched.is_err());
    assert!(started.elapsed() < std::time::Duration::from_secs(10), "the cancel waited out the pull");
    let calls = std::fs::read_to_string(&argv).unwrap();
    assert!(calls.contains(&format!("rm -f {}", spec.name)), "{calls}");
}
```

`both_tokens()` is the file's `token()`, renamed in Task 7.

- [ ] **Step 7: k8s reattaches by Job name**

`src/runner/kubernetes.rs`: `reattach(RoundHandle::Kubernetes { job })`
builds `KubernetesRound::named(self.clone(), job.clone())`, waits for its pod
with `poll_until_started` (a finished pod counts as started), and follows its
log from the beginning — `LogReadPosition::default()`; the collector's own
seq position drops what it already has. `handle()` is
`RoundHandle::Kubernetes { job: self.name.clone() }`.

`tests/kubernetes_runner.rs`: add
`a_kubernetes_round_is_reattached_by_its_job_name`, shaped like the docker
test above, against the file's existing fake `kubectl` — launch, take the
handle, drop the round, `reattach`, collect, and assert the verdict passed
and the output log holds the agent's line once.

- [ ] **Step 8: The daemon folds its root on start, and leaves rounds running when it stops**

`src/daemon/fleet.rs`:

```rust
//! What a daemon starting on a root owes the jobs it finds there — decided
//! from each job's log alone.

use super::dispatch::JobAddress;
use crate::event::EventLog;
use crate::paths::{self, JobPaths, RepoKey};
use crate::report::JobReport;
use crate::runner::RoundHandle;
use crate::state::JobState;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resumption {
    /// Asked for and never started: back in the queue.
    Requeue,
    /// Launched and never judged: collect it from where the log left off.
    Reattach { round: u32, handle: RoundHandle },
    /// Started but never recorded as launched: the daemon stopped mid-launch,
    /// and there is no handle to find the round by.
    LostWhileLaunching { round: u32 },
}

impl Resumption {
    #[must_use]
    pub fn for_report(report: &JobReport) -> Option<Resumption> {
        match (report.state, &report.launched) {
            (JobState::Queued, _) => Some(Resumption::Requeue),
            (JobState::Running, Some(handle)) => Some(Resumption::Reattach {
                round: report.rounds,
                handle: handle.clone(),
            }),
            (JobState::Running, None) => Some(Resumption::LostWhileLaunching {
                round: report.rounds,
            }),
            (JobState::Pending | JobState::Passed | JobState::Failed, _) => None,
        }
    }
}

/// Every job under `root`, with its report and the address the daemon finds
/// it by. A job directory is any directory under `<root>/jobs` holding an
/// `events.jsonl`; its key comes from the remote its log names.
///
/// # Errors
///
/// When the jobs directory cannot be walked or a log cannot be read.
pub fn jobs_under(root: &Path) -> anyhow::Result<Vec<(JobAddress, JobReport)>> {
    job_dirs_below(&root.join("jobs"))?
        .into_iter()
        .filter_map(|dir| address_and_report(dir).transpose())
        .collect()
}

fn job_dirs_below(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    match std::fs::read_dir(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
        Ok(entries) => entries
            .map(|entry| {
                let path = entry?.path();
                match (path.is_dir(), path.join("events.jsonl").is_file()) {
                    (true, true) => Ok(vec![path]),
                    (true, false) => job_dirs_below(&path),
                    (false, _) => Ok(Vec::new()),
                }
            })
            .collect::<std::io::Result<Vec<_>>>()
            .map(|nested| nested.into_iter().flatten().collect()),
    }
}

/// `None` for a directory whose name is not a job id or whose log names no
/// remote — not a job this daemon can do anything for.
fn address_and_report(dir: PathBuf) -> anyhow::Result<Option<(JobAddress, JobReport)>> {
    let Some(id) = dir
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.parse::<u64>().ok())
    else {
        return Ok(None);
    };
    let paths = JobPaths { id: id.into(), dir: dir.clone() };
    let report = JobReport::from_events(id, &EventLog::read(paths.events())?);
    let Some(key) = report.remote_url.as_deref().and_then(|url| RepoKey::from_remote_url(url).ok())
    else {
        return Ok(None);
    };
    let jobs_dir = dir.parent().map(Path::to_path_buf).unwrap_or_default();
    Ok(Some((JobAddress { key, jobs_dir, id: id.into() }, report)))
}
```

(`paths` is imported for `JobPaths`; drop the `self` if nothing else uses
the module.)

`src/daemon/dispatch.rs`:

- `run_queued_round` builds the spec with `paths.frames(round)`, launches,
  records `RoundLaunched { handle: running.handle() }`, and collects with
  `&paths.position(round)`.
- Add `reattach_round(serving, address, round, handle, cancel)`: registers
  the cancel token, counts itself in `running`, calls
  `serving.daemon.runner.reattach(&handle)`, and collects with
  `paths.position(round)`; a reattach that fails records
  `RoundFailed { reason: format!("could not find the round again after a restart: {e}") }`.
- `JobQueue::stop_rounds_and_wait` is deleted.

`src/daemon/mod.rs` — `serve` resumes before it listens, and no longer stops
rounds when it goes:

```rust
    let (queue, pending) = dispatch::JobQueue::new();
    let serving = Arc::new(Serving { daemon, repos: dispatch::RepoLocks::default(), queue });
    resume_jobs_under_root(&serving)?;
    let dispatcher = tokio::spawn(dispatch::dispatch_until_closed(Arc::clone(&serving), pending));

    let served = axum::serve(listener, api::router(Arc::clone(&serving)))
        .with_graceful_shutdown(shutdown)
        .await;
    // Rounds are left running: each finishes without us, and the next daemon
    // on this root reattaches to it.
    dispatcher.abort();
```

```rust
/// Give every job under the root what it is owed: reattach, close, or
/// requeue in the order they were asked for.
fn resume_jobs_under_root<R: Runner + Send + Sync + 'static>(
    serving: &Arc<Serving<R>>,
) -> anyhow::Result<()> {
    let owed: Vec<(JobAddress, JobReport, Resumption)> = fleet::jobs_under(&serving.daemon.root)?
        .into_iter()
        .filter_map(|(address, report)| {
            Resumption::for_report(&report).map(|owed| (address, report, owed))
        })
        .collect();
    let (requeued, others): (Vec<_>, Vec<_>) = owed
        .into_iter()
        .partition(|(_, _, owed)| matches!(owed, Resumption::Requeue));

    others.into_iter().try_for_each(|(address, _, owed)| match owed {
        Resumption::Reattach { round, handle } => {
            dispatch::spawn_reattached(serving, address, round, handle);
            Ok(())
        }
        Resumption::LostWhileLaunching { .. } => EventLog::open_append(address.paths().events())
            .and_then(|mut log| {
                log.append(EventKind::RoundFailed {
                    reason: "the daemon stopped while this round was launching, so it cannot be \
                             found again — check the runner for a stray round, then submit again"
                        .into(),
                })
            })
            .map(|_| ()),
        Resumption::Requeue => Ok(()),
    })?;

    let mut requeued = requeued;
    requeued.sort_by_key(|(_, report, _)| report.requested_at);
    requeued
        .into_iter()
        .for_each(|(address, _, _)| serving.queue.enqueue(address));
    Ok(())
}
```

`dispatch::spawn_reattached` is the tracker-spawned wrapper around
`reattach_round`, with the same bookkeeping as a dispatched round (cancel
table, `running` count up and down). It runs whatever the cap: a round
already running is not something the cap can prevent, only count.

- [ ] **Step 9: The end-to-end tests**

`tests/fixtures/gated-agent.sh`:

```bash
#!/usr/bin/env bash
# Waits until the file $GATE exists, so a test decides when the round ends.
set -euo pipefail
echo "gated-agent: $1"
for _ in $(seq 1 600); do [ -e "$GATE" ] && break; sleep 0.05; done
printf '%s\n' "$1" > agent-output.txt
```

`tests/daemon_jobs.rs` — replace `stopping_the_daemon_cancels_its_rounds_and_records_why`
with:

```rust
#[tokio::test]
async fn a_round_outlives_its_daemon_and_the_next_one_collects_it() {
    let gate = tempfile::tempdir().unwrap().keep().join("open");
    let gate_env = [("GATE", gate.to_str().unwrap())];
    let mut fx = Fixture::running("gated-agent.sh", &[], &gate_env).await;
    fx.assembly().args(["submit", "--prompt", "x"]).assert().success();
    wait_until_launched(&fx.job_dir(1));

    assert!(fx.daemon.take().unwrap().stop().success());
    std::fs::write(&gate, "").unwrap();
    let fx = fx.with_daemon(&[], &gate_env);

    let report = wait_for_verdict(&fx.job_dir(1));
    assert_eq!(report.state, JobState::Passed);
    let log = std::fs::read_to_string(fx.job_dir(1).join("job.log")).unwrap();
    assert_eq!(log.matches("gated-agent: x").count(), 1, "{log}");
    assert_eq!(fx.on_origin(&["show", "al/job-1:agent-output.txt"]), "x");
}

#[tokio::test]
async fn jobs_left_queued_run_after_a_restart_in_the_order_they_were_asked_for() {
    let gate = tempfile::tempdir().unwrap().keep().join("open");
    let gate_env = [("GATE", gate.to_str().unwrap())];
    let mut fx = Fixture::running("gated-agent.sh", &["--max-jobs", "1"], &gate_env).await;
    (0..3).for_each(|i| {
        fx.assembly().args(["submit", "--prompt", &format!("job {i}")]).assert().success();
    });
    wait_until_launched(&fx.job_dir(1));

    assert!(fx.daemon.take().unwrap().stop().success());
    std::fs::write(&gate, "").unwrap();
    let fx = fx.with_daemon(&["--max-jobs", "1"], &gate_env);

    let started: Vec<chrono::DateTime<chrono::Utc>> = (1..=3)
        .map(|id| {
            assert_eq!(wait_for_verdict(&fx.job_dir(id)).state, JobState::Passed);
            round_started_at(&fx.job_dir(id))
        })
        .collect();
    assert!(started[1] < started[2], "job 3 started before job 2: {started:?}");
}

#[tokio::test]
async fn a_round_lost_while_launching_is_closed_on_restart() {
    let fx = Fixture::running("fake-agent.sh", &[], &[]).await;
    fx.assembly().args(["submit", "--prompt", "x"]).assert().success();
    wait_for_verdict(&fx.job_dir(1));
    // A daemon that died between starting a round and launching it leaves
    // exactly this: a request and a start, and nothing after.
    let job = fx.job_dir(1);
    let mut log = EventLog::open_append(job.join("events.jsonl")).unwrap();
    log.append(requested_again(&fx)).unwrap();
    log.append(EventKind::RoundStarted { round: 2 }).unwrap();
    let mut fx = fx;
    fx.daemon.take().unwrap().stop();
    let fx = fx.with_daemon(&[], &[]);

    let report = wait_for_verdict(&job);
    assert!(report.detail.unwrap_or_default().contains("while this round was launching"));
}
```

with `wait_until_launched` (fold until `report.launched.is_some()`) added to
`tests/support/daemon.rs` beside `wait_until_running`, `round_started_at`
(the `at` of the job's last `RoundStarted`) and `requested_again` (a
`RoundRequested` copying the job's first one) as helpers in the test file.
`tempfile::TempDir::keep` leaks the directory on purpose: the gate must
outlive the daemon's `Drop`; if your `tempfile` names it `into_path`, use
that.

Tests that leave a sleeping agent running when they end cancel it first
(`assembly cancel 1`), since stopping the daemon no longer does.

- [ ] **Step 10: Run the gate**

Run: `just check`
Expected: PASS. Run it three times: reattach tests race a daemon's exit
against a round's, and must pass whichever wins.

- [ ] **Step 11: Commit**

```bash
jj describe -m "feat(daemon): a restarted daemon reattaches to the rounds it left running

Every round records a handle when it launches. The local runner starts
run in its own session writing frames to a file, docker creates then
starts then follows its container, and k8s finds its pod by Job name,
so any daemon can resume any round's stream; the collector skips what
it already routed. On start the daemon reattaches to launched rounds,
closes ones lost mid-launch, and requeues the rest in order. Stopping
the daemon stops no job. A docker round cancelled mid-pull leaves no
container: closes accepted risk 12.

Tests: <count>."
jj new
```

---

### Task 13: `CLAUDE.md`, the demo and the plan's status follow the new boundary

**Files:**
- Modify: `CLAUDE.md`, `justfile` (`demo`), `docs/superpowers/plans/2026-09-28-software-factory-f3.md` (status line), `docs/superpowers/specs/2026-09-11-software-factory-v2.md` (F3 ✅)

- [ ] **Step 1: `CLAUDE.md`**

- The opening paragraph: "A Rust CLI and daemon that run coding-agent jobs
  — a repo, a ref, and a prompt — as branches, decide whether each round
  passed with `verify`, and open a pull request when one did."
- The current milestone line names this plan.
- **Vocabulary**: a job is "an id, its branch `al/job-{id}`, and one event
  log under the daemon's root, whose first `RoundRequested` is its
  identity". A round: "`run` is one round; each `submit --job` adds one".
  The paragraph about `job-exec` and `ASSEMBLY_JOB` keeping their names is
  replaced by: "A runner launches `assembly run` — the command a person
  types — so a round's command line is its reproduction."
- **Style → Where loops are still correct**: the daemon's dispatcher is now
  the headline case — "`daemon::dispatch::dispatch_until_closed` takes one
  queued job at a time and waits for a free slot before looking at the next;
  that ordering *is* the loop." Remove "there is no scheduler loop any more".
- **Cost**: the daemon runs many jobs at once now; the collections are still
  a handful per job, and scheduling contention is the cap, proven by
  observation in `tests/daemon_jobs.rs`.
- **Testing**: the concurrency paragraph is no longer forward-looking —
  point at `the_daemon_never_runs_more_rounds_at_once_than_its_cap` and its
  probe agent. Add: "Tests that run the binary set `ASSEMBLY_ROOT` to a
  tempdir; tests that need a daemon start one with
  `support::daemon::RunningDaemon`."
- **Invariants**: job ids are claimed by creating `al/job-N` on the remote
  (`claim::claim_job`), never allocated locally; the factory never writes to
  a user's repository, working tree or `.git` — job state and every fetch
  live under the daemon's root (`paths::RepoKey`); the event log is still
  append-only, and a round's `position` file is the one thing under a job
  directory that is rewritten.

- [ ] **Step 2: The demo**

`justfile` `demo` runs one job end to end with `run` — no daemon needed —
and drops the `status` line (`run` keeps no state):

```
    "$bin" run --prompt "make a change" || true
    echo "demo job left in $dir (its branch is on $dir/origin.git)"
```

The demo repository needs an origin now: `git init --bare "$dir/origin.git"`,
`git remote add origin "$dir/origin.git"`, and `git push -q origin main`
after its commit.

- [ ] **Step 3: Status lines**

This plan's `**Status:**` becomes "planned 2026-09-28; implemented." and the
spec's F3 milestone row gains ✅.

- [ ] **Step 4: Run the gate and the whole stack**

Run: `just check && just verify-stack`
Expected: every revision `ok`, the test count climbing except at Tasks 1, 5
and 7.

- [ ] **Step 5: Commit**

```bash
jj describe -m "docs: CLAUDE.md and the demo follow the new boundary

Vocabulary, loops, cost, testing and invariants describe the daemon,
run as the whole job, claims on the remote and state under the root.
The demo runs a job with run. F3 is marked done.

Tests: <count>."
jj new
```
