# Arch 06 — a remote is a value, and "origin" has one meaning

Finding #6 of `.architecture-reports/2026-10-17-architecture-review.html`,
grilled with the user on 2026-10-29. **Status:** built and reviewed.
Picked up from
`docs/superpowers/plans/2026-10-20-architecture-handoff.md`.

## Goal

Where a job clones from and pushes to is one value, `remote::Remote`, that
owns every URL rule. One function resolves what the person pointed at with
`--repo`. Inside the scratch clone, only the workspace names the clone's
remote, so "origin" has one meaning.

## Why this shape

Evidence checked at `smnununx`. Waves 1 and 2 (#4, #5, #11, #9, #7) moved
code in `run.rs`, `paths.rs`, `payload.rs`, `runner/mod.rs` and
`daemon/*`, so the building agent checks every line reference again on its
base. In particular, #5 changed `RepoKey::from_remote_url` (`@local` host,
`.git` stripped once), and #7 changed `LaunchSpec::for_round`'s inputs.

| Fact | Where (at `smnununx`) |
|---|---|
| URL rules in five modules: `https_equivalent`, `is_path_on_this_machine`, `remote_to_clone` (`payload.rs`); `scp_like_parts`, `split_at_host_colon`, `reachable_from_anywhere` (`git.rs`); `RepoKey::from_remote_url` (`paths.rs`); the `file://`/https rewrite (`runner/mod.rs`, `LaunchSpec::for_round`) | `payload.rs:24-123`, `git.rs:302-323`, `paths.rs:85`, `runner/mod.rs:58-65` |
| Three more local-path tests: `paths.rs` handles `file://` and `starts_with('/')`; `git.rs:303` tests "names a host" separately; `runner/mod.rs:60` tests `is_absolute()` | same |
| Host extraction and the authority/path split are copied in `payload.rs` and `paths.rs` | `payload.rs:26-33`, `paths.rs:93-95` |
| "Is `--repo` a checkout or a URL?" is decided three times, then each calls `remote_to_clone(&checkout, DEFAULT_REMOTE)`; "HEAD is detached …" is written twice | `run.rs:414`, `locate.rs:43`, `submission.rs:62`, `run.rs:427`, `submission.rs:74` |
| `run.rs` uses `DEFAULT_REMOTE` (the user's remote) for three things inside the clone, where `CLONE_REMOTE` is meant: `claim_job`, `remote_name` (which becomes `BranchPushed.pushed_to`), and the "no branch on 'origin'" text | `run.rs:90`, `:194`, `:209`; `workspace.rs:40-44`, `:110`, `:177` |
| The daemon passes a URL where `git.rs` and `claim.rs` name a `remote` | `submit.rs:82`, `:130`, `:168` |
| `RoundPayload.remote_url` is written and never read | `payload.rs:57`, `:92` |
| `BranchPushed.pushed_to` is always `"origin"`; nothing in `src/` reads it | `round.rs:379`, `:401`; `event.rs:45` |

## Success criteria

1. **One value owns the URL rules.** `src/remote.rs` defines `Remote`,
   made by parsing once. It owns: whether it is a path on this machine or
   reachable from anywhere, its HTTPS form, its `RepoKey`, and the form a
   launch passes (`file://` for a local path, HTTPS in a container). A grep
   outside `src/remote.rs` finds no `://` parsing, no `scp_like_parts`, no
   `split_at_host_colon`, and no `is_absolute()` test on a URL.
   `payload.rs` and `paths.rs` hold no URL rules.
2. **One resolution of `--repo`.** One function decides "checkout or URL"
   and the ref that implies; `run`, `submit` and `locate` call it. The
   "HEAD is detached — name the ref to start from with --ref" text exists
   once.
3. **"origin" has one meaning.**
   - `run.rs` names no remote for work inside the clone; the workspace
     owns the clone's remote name.
   - The daemon passes a `Remote` (its URL) where `git.rs` wants one.
     `git.rs` keeps `&str` for a remote, with the parameter named
     `name_or_url` and a doc line saying git accepts either.
   - `RoundPayload.remote_url` is deleted.
   - The "no job branch" message is one wording, naming the URL:
     `job {id} has no branch on {url} — …`, for `run --job` and
     `submit --job` both.
   - `BranchPushed.pushed_to` holds the URL the clone pushed to.
4. **No spelling changes.** A table test in `tests/remote.rs`, written
   first against the base's code, pins for each spelling what is accepted,
   its HTTPS form, its `RepoKey`, and its launch form: absolute paths,
   `file://`, `https://`, `ssh://`, scp-like (`git@host:o/r`), IPv6 hosts,
   ports, user info, relative paths, trailing `/` and `.git`. It passes
   unchanged after the change. The URL tests in `tests/runner.rs`,
   `tests/paths.rs` and `tests/git.rs` that it duplicates move into it.
5. `just check` is clean and the test count rises.
   `just verify-stack` passes after the merge (the supervisor runs it).

## Stated behaviour change

- `run --job`'s "no branch" message names the URL instead of `'origin'`.
- New `BranchPushed` events record the URL in `pushed_to` instead of
  `"origin"`. Old logs keep `"origin"`; nothing reads the field, so they
  still fold the same.

### Amendment: decisions 7 and 8

Found by the building agent's spelling table; decided by the user on
2026-10-30.

- **Decision 7, `pushed_to`:** the URL as the clone pushed to it, user
  info included (option A). A URL the person typed with a password in it
  (`https://user:TOKEN@host/...`) is then written into `pushed_to`, as it
  already is into `RoundRequested.remote_url` and a container launch's
  `--repo=`. The new exposure is in a hand-typed `run`'s frames only.
  Removing credentials from URLs is a separate, later item.
- **Decision 8, ports in the key:** a non-default port is part of the
  `RepoKey`. Default ports (https 443, http 80, ssh 22, git 9418) are
  dropped, so the keys that are shared today stay shared
  (`ssh://host:22/o/r`, `git@host:o/r`, `https://host/o/r`). Any other
  port stays in the host segment as `host+PORT`. `+` is not a keyable
  character, so no real host can equal it. `RepoKey::from_dir_name`
  accepts the same form back. Examples: `https://h:8443/o/r` →
  `h+8443~o~r`; `ssh://h:2222/o/r` → `h+2222~o~r`. This is a stated
  behaviour change: the spelling table's rows with a non-default port
  change their key, and no other row changes. A daemon whose root holds
  jobs for such a remote no longer finds them (#5's "ignore old layout"
  rule covers this: no migration).

## In scope

- New `src/remote.rs` and `tests/remote.rs`.
- `src/payload.rs`, `src/git.rs`, `src/paths.rs`, `src/runner/mod.rs`,
  `src/run.rs`, `src/locate.rs`, `src/submission.rs`, `src/workspace.rs`,
  `src/round.rs` (`pushed_to`), `src/claim.rs`, `src/daemon/*`, and the
  tests that call what moves.

## Out of scope

- Typed SHAs, refs and job branches, and argument order in `git.rs`: #10.
- `RUNS_IN_A_CONTAINER` and what it decides: #8. The launch-form rule moves
  into `Remote` with the same input (in a container or not).
- Any spelling that today's code accepts and the new code would refuse,
  or the reverse, and any change of result other than decision 8's port
  rule. If the table test shows another spelling whose current result
  looks like a bug, stop and ask: do not fix it here.
- Credentials in remote URLs (decision 7), IPv6 hosts staying unkeyable,
  and the relative `run --repo=../bare` path: known open, not fixed here.
- The event schema's fields (only `pushed_to`'s value changes).

## Constraints

- Everything in `CLAUDE.md`, including one jj change that builds, tests,
  lints and formats on its own; tests through the public API; no new
  `#[cfg(test)]` in `src/`; error types say what to do.
- The factory never writes to a user's repository.

## Decisions

| # | Decision | By |
|---|---|---|
| 1 | All three: the value, one `--repo` resolution, the "origin" fix (with the dead field) | user (Q1) |
| 2 | Done = criteria 1–5, with the spelling table written first | user (Q2) |
| 3 | `src/remote.rs`; `git.rs` keeps `&str`, named `name_or_url` | user (Q3) |
| 4 | The "no job branch" message names the URL | user (Q4) |
| 5 | `BranchPushed.pushed_to` holds the URL | user (Q5) |
| 6 | Method names on `Remote`; whether `RepoKey::from_remote_url` stays as a thin call into `Remote` or moves | delegated |
| 7 | `pushed_to` records the URL as pushed, user info included (see "Amendment: decisions 7 and 8") | user (mid-build) |
| 8 | A non-default port is kept in the key as `host+PORT`; default ports are dropped (see the amendment) | user (mid-build) |
| 9 | Port detail: the default port is decided per scheme (`http://h:443` keeps `+443`); a non-numeric port is dropped, as before | user (mid-build, confirming the agent's reading) |
| 10 | The two clone-internal messages that name `'origin'` (`workspace::pin_in_clone`, and `git::pinned` when it runs inside the clone) name the URL instead, by decision 4's rule | user (mid-build) |
| 11 | Keep what decision 10's build did beyond the two messages: `CLONE_REMOTE` is gone, and every git call inside the scratch clone names the remote by its URL (so the claim errors and git's own command text name it too, and an agent cannot redirect the push by editing `remote.origin.url`). Before the merge, add a test, with no network, that a credential helper set in the clone's config is used for a push by URL (for example a fake helper that records that git asked it) | user (mid-build) |

## Assumptions

- The pushed URL holds no credential: the clone authenticates through a
  credential helper, not a token in the URL. The agent checks this before
  writing the URL into the event log, and stops and asks if a token can
  appear there.

## Open questions

None.

## Implementation notes

Order of work: tests first, then code.

1. **Tests.** The spelling table against the base's code, green before any
   code moves. Then the tests for the new message text and `pushed_to`.
2. **Code.** `Remote` and its rules; then callers (`paths`, `runner`,
   `payload`, `git`); then the `--repo` resolution; then the clone's
   remote, the dead field, the message and `pushed_to`.
3. **Docs.** Update this brief's status line. Do not edit the handoff: the
   supervisor updates it after the merge. Apply the de-ai standard to the
   added lines.
4. **Checks.** `just check` in the workspace. Record the test count in the
   commit body.

## jj

#6 is wave 3 of the handoff's "Waves", on the merged waves 1 and 2. Build
in the working-copy change of the jj workspace the supervisor made for it.
Describe it as:

```
refactor(remote): a remote is a value, and "origin" has one meaning

<why, the stated behaviour change, and: Tests: N (was M).>
```
