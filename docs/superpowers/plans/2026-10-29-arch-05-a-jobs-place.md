# Arch 05 — a job's place under the root has one owner, and repositories cannot collide

Finding #5 of `.architecture-reports/2026-10-17-architecture-review.html`,
grilled with the user on 2026-10-29. **Status:** approved by the user
on 2026-10-29; built and reviewed. Picked up from
`docs/superpowers/plans/2026-10-20-architecture-handoff.md`.

## Goal

A job's place under the root has one owner, made from a `RepoKey` and a
`JobId`, and no two repositories can share a directory. The jobs tree and
the cache tree are flat: one directory per repository, named so that no
repository's directory is inside another's.

## Why this shape

Evidence checked against `knyktlrn` on 2026-10-29.

| Fact | Where |
|---|---|
| Two types describe one place: `paths::JobPaths { id, dir }` and `dispatch::JobAddress { key, jobs_dir, id }`. `jobs_dir` is always `key.jobs_dir(root)` but is stored, and is part of `Hash`/`Eq`, so it is part of the cancel-table key | `src/paths.rs:196`, `src/daemon/dispatch.rs:27` |
| `JobAddress` is built in three places, and fleet builds its `jobs_dir` from `dir.parent()` while its key comes from the log. Nothing checks that they agree | `src/daemon/submit.rs:113`, `src/daemon/api.rs:119`, `src/daemon/fleet.rs:103-116` |
| "Read the log, then fold" is written four times, each with its own error handling | `src/locate.rs`, `src/daemon/dispatch.rs:43`, `src/daemon/fleet.rs:100`, `src/daemon/submit.rs` |
| `JobReport.id` and `from_events`'s `id` are `u64` | `src/report.rs:35`, `:187` |
| `RepoKey::jobs_dir` folds the segments with no terminator, so `/x/a.git`'s job 7 is `/x/a/7.git`'s jobs directory. `create_job` then fails with `AlreadyExists` and `submit` says the branch was gone; fleet stops at the first `events.jsonl` and hides one repository's jobs | `src/paths.rs:120`, `src/daemon/fleet.rs:68` |
| Host `local` is not reserved: `https://local/x/a` keys as `/x/a.git` | `src/paths.rs:84` |
| `/x/a.git/b`'s cache is inside `/x/a.git`'s cache | `src/paths.rs:131` |
| `trim_end_matches(".git")` strips repeatedly: `/x/a.git.git` keys as `/x/a` | `src/paths.rs:101` |
| Known open in F3 | `docs/superpowers/plans/2026-09-28-software-factory-f3.md:19` |

## Success criteria

1. **No collisions.** Tests under one root, each through the public API:
   - job 7 of `/x/a.git` and a job of `/x/a/7.git` are both created, both
     are listed, and a daemon restart (fleet) resumes both;
   - `https://local/x/a` and `/x/a.git` have different keys and
     directories;
   - `/x/a.git/b`'s cache is not inside `/x/a.git`'s cache;
   - `/x/a.git.git` and `/x/a` have different keys.
2. **One owner.** `paths::JobPlace { key: RepoKey, id: JobId }` is made only
   from a key and an id, and stores nothing it can derive. `JobAddress` is
   deleted. A job directory is reached only through `JobPlace` (and the
   test helpers that make a `JobPaths` for a runner).
3. **One fold.** `JobPaths::report()` reads the log and folds it. locate,
   dispatch, fleet, submit and the test support (`tests/support/daemon.rs`,
   `tests/collect.rs` where it reads a real log) use it. No other code in
   `src/` calls `EventLog::read` and then `JobReport::from_events`.
4. **Fleet lists by the creating rule.** It lists `<root>/jobs/<repo>/<id>`
   (two levels, no recursion) and gets the key from the repository
   directory's name. Names that do not parse are skipped.
5. `just check` is clean and the test count rises from 373.
   `just verify-stack` passes after the merge (the supervisor runs it).

## Stated behaviour change

- The on-disk layout of `<root>/jobs` and `<root>/repos` changes. A daemon
  does not read jobs in the old layout, and clones a fresh bare cache. No
  migration. Old roots must be deleted by hand.
- `RepoKey` values change: a local path's host is `@local`, and `.git` is
  stripped once. `RepoKey`'s `Display` may show `@local/...`.

## In scope

- **`src/paths.rs`.**
  - `RepoKey::from_remote_url`: strip `.git` once; the host of a local path
    (`/...` or `file://`) is the marker `@local`. The marker is not a
    keyable segment, so no real host can equal it; it is set by code, not
    validated as a segment.
  - `RepoKey::dir_name()`: the segments joined with `~`. `~` is not a
    keyable character, so the join is unambiguous.
    `RepoKey::from_dir_name(&str) -> Option<RepoKey>`: the inverse,
    validated the same way.
  - `jobs_dir(root)` = `<root>/jobs/<dir_name>`;
    `repo_cache(root)` = `<root>/repos/<dir_name>.git`.
  - `JobPlace { key, id }`, deriving `Debug, Clone, PartialEq, Eq, Hash`.
    `paths(root) -> JobPaths`, `create(root) -> io::Result<JobPaths>`
    (`AlreadyExists` as today), `open(root) -> io::Result<JobPaths>`
    (`NotFound` as today). `create_job`/`open_job` move behind it or are
    deleted.
  - `JobPaths::report() -> anyhow::Result<JobReport>` and
    `JobPaths::append(EventKind) -> io::Result<()>`. `JobPaths` keeps its
    path methods and its public fields.
  - `existing_job_ids` / `latest_job_id` keep working on one repository's
    jobs directory.
- **`src/report.rs`.** `JobReport.id: JobId`; `from_events(id: JobId, ..)`.
  The fold itself does not change.
- **`src/locate.rs`.** Keeps only "what did the person mean?": it resolves
  a repository to a key and a job id to a `JobPlace`, and reads the report
  through `JobPaths::report`.
- **`src/daemon/*`.** `JobAddress` → `JobPlace` in the queue, the cancel
  table, `submit`, `api` and `fleet`. Fleet lists two levels.
- **Tests.** The collision tests (criterion 1), `tests/paths.rs` updated
  for the new key values and paths, `tests/fleet.rs` for the new walk, and
  the `from_events` callers in tests passing a `JobId`.
- **F3 plan.** Mark the known-open collision item at
  `2026-09-28-software-factory-f3.md:19` as fixed by this change.

## Out of scope

- Migration of old roots, or detection of an old layout.
- Deleting the untracked `.assembly/jobs/` in the repository.
- #6 (where a job points; the URL rules in `payload.rs`, `git.rs`,
  `runner/mod.rs`). `from_remote_url` changes only in the two ways above.
- #7 (the four `Option`s on `JobReport`). Fleet no longer reads
  `remote_url` to find a key, but other readers stay.
- Runner signatures: runners keep taking `&JobPaths`. `src/runner/*`
  should not change. If it must, stop and ask: #11 is building in
  `src/runner/kubernetes.rs` at the same time.
- `src/run.rs`, `src/round.rs`, `src/delivery.rs`, `src/workspace.rs`: #4
  is building there at the same time. The only expected touch in `run.rs`
  is the `from_events` id argument (one line). If more is needed, stop and
  ask.

## Constraints

- Everything in `CLAUDE.md`, including one jj change that builds, tests,
  lints and formats on its own; tests through the public API; no new
  `#[cfg(test)]` in `src/`; append-only event log; `from_events` stays pure.
- Job ids are never user input; branch names are unchanged
  (`al/job-{id}`).
- The factory never writes to a user's repository.

## Decisions

| # | Decision | By |
|---|---|---|
| 1 | Fix both the collision and the single owner | user (Q1) |
| 2 | Done = criteria 1–5 | user (Q2) |
| 3 | All four collisions are in scope: jobs dir, `local` host, cache nesting, repeated `.git` strip | user (Q3) |
| 4 | Ignore old-layout roots: no migration, no detection | user (Q4) |
| 5 | Flat layout: one directory per repository, segments joined with `~`; local host marker `@local`; strip `.git` once | user (Q5) |
| 6 | `paths::JobPlace { key, id }` replaces `JobAddress`; `JobPaths` stays the opened directory and gains `report()` and `append` | user (Q6) |
| 7 | `JobReport.id` and `from_events`'s argument become `JobId` | user (Q7) |
| 8 | Fleet gets the key from the directory name (`RepoKey::from_dir_name`) | user (Q8) |
| 9 | `RepoKey`'s `Display` format, and exact names of helpers not named above | delegated |
| 10 | Update the design spec's layout lines to the flat layout | user (mid-build, #5 review finding #2) |

## Assumptions

- No state root that matters exists (none was found on this machine).
- `~` and `@` are safe in directory names on the platforms the factory runs
  on (macOS, Linux, the job container).
- Tests may still make a `JobPaths` by hand for a runner (`tests/runner.rs`,
  `tests/support/mod.rs`); that is not a second way to place a real job.

## Open questions

None.

## Implementation notes

Order of work: tests first, then code.

1. **Tests.** The four collision tests (criterion 1) fail first. Then
   update `tests/paths.rs` and `tests/fleet.rs` for the new layout.
2. **Code.** `RepoKey` (strip once, `@local`, `dir_name`, `from_dir_name`,
   `jobs_dir`, `repo_cache`); `JobPlace` and `JobPaths::report`/`append`;
   `JobReport.id`; then locate, the daemon, fleet.
3. **Docs.** Update this brief's status line and the F3 known-open item.
   Do not edit the handoff: the supervisor updates it after the merge.
   Apply the de-ai standard to the added lines.
4. **Checks.** `just check` in the workspace. Record the test count in the
   commit body.

## jj

#5 is in wave 1 of the handoff's "Waves". Build in the working-copy change
of the jj workspace the supervisor made for it. The supervisor puts it in
the stack at the merge. Describe it as:

```
jj describe -m "fix(paths): a job's place has one owner, and repositories cannot collide

<why, the stated behaviour change, and: Tests: N (was 373).>"
```
