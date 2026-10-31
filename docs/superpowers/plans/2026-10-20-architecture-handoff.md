# Architecture improvement — handoff

A living tracker for working through the eleven findings of the 2026-10-17
architecture review. Update the progress table whenever a finding moves.

## Where the review lives

The full report is
`.architecture-reports/2026-10-17-architecture-review.html`. It is in jj
change `prwrkokz`, at the top of the stack, which holds nothing else and is
not meant to land, so it is not in the working tree of the changes below it.
To read it: `jj file show -r prwrkokz .architecture-reports/2026-10-17-architecture-review.html`.
The finding summaries below are enough to start each grill.

## The process the user chose

#1–#3 were done one at a time: grill, build, then move on. For #4–#11 the
user chose a different process: **one supervising agent, and building
agents that run at the same time, in waves.**

### Roles

- **The supervisor** is the agent that reads this handoff. It:
  - grills each finding with the user and gets each brief approved;
  - makes the jj workspaces;
  - starts the building agents;
  - takes their questions to the user;
  - merges their changes into one stack at the end.

  The supervisor does not build findings itself.
- **A building agent** builds one finding, in one jj workspace, with the
  user's `/implement` skill. It never talks to the user. It sends every
  question to the supervisor.

### 1. Grill (the supervisor, with the user)

A building agent needs an approved brief, and only the user can approve
one. So the supervisor grills before it builds:

- Use the `grill-me` skill. One question per message, each with a
  recommended answer and a one-line reason.
  - Settle the outcome and the definition of done before design, and
    design before risks.
  - Look facts up in the code yourself; put decisions to the user.
- Start from the finding's pre-grill notes
  (`2026-10-29-arch-NN-*.md`). Check the evidence against the wave's base
  first, because the waves before it move lines.
- Finish with a teach-back. Then turn the notes file into the brief (Goal,
  Success criteria, In and Out of scope, Constraints, Decisions,
  Assumptions, Open questions, Implementation notes), and get explicit
  approval.
- Grill every finding of a wave before that wave starts. While one wave
  builds, grill the next wave's findings, so the user's time and the
  agents' time overlap.

### 2. Workspaces (the supervisor)

Each building agent gets its own jj workspace. Use jj workspaces, not git
worktrees, and not the Agent tool's `isolation: "worktree"`.

- Make it outside the repository, so the main workspace never snapshots
  it:

  ```
  jj workspace add /Users/harry-mac/code-stuff/assembly-line-arch-NN \
    --name arch-NN -r <wave base>
  ```

  Its working-copy change is a new, empty change on the wave base. The
  agent builds in that change.
- Each workspace has its own Cargo target directory:
  `CARGO_TARGET_DIR=/tmp/assembly-line-target-arch-NN`. One shared target
  directory makes cargo reuse another tree's `assembly-line` artifacts,
  and a correct change then looks broken (see CLAUDE.md, "Verifying the
  stack"). Concurrent builds also block each other on its lock.
- Only the main workspace is colocated with git. In a secondary workspace,
  `git` commands that need the repository (`git archive`, so also
  `just verify-stack`) do not work. Run `just verify-stack` only in the main
  workspace, after the merge. Never set `GIT_DIR` to work around this.
  The user's memory records that a leaked `GIT_DIR` once corrupted the main
  repository's `.git`.

### 3. Build (the building agents, at the same time)

Start one agent per finding in the wave, all in one message, so they run
concurrently. Use `subagent_type: "general-purpose"`. Each agent's prompt
gives:

- the workspace path, its `CARGO_TARGET_DIR`, and the wave base's change
  id;
- the path of the approved brief;
- an instruction to invoke the `implement` skill (the user's `/implement`)
  on that brief, and to follow it completely:
  - baseline first;
  - tests first;
  - the de-ai standard on added lines;
  - an adversarial review in a fresh subagent;
  - fix its findings;
- the rules in "Rules for building agents" below.

**Rules for building agents.**
- Work only inside your workspace. Run every command there (for example
  `cd <workspace> && just check`), and use absolute paths for files.
- Build in the workspace's working-copy change. Describe it as the brief's
  `jj` section says. The commit body records the test count. Do not make
  other changes, and do not rebase, squash into or edit any change outside
  your workspace.
- Edit files with the Edit and Write tools only. No shell edits.
- Update your own brief's status line. Do not edit this handoff: the
  supervisor updates it after the merge, so parallel agents do not
  conflict on it.
- Never push.
- **When something comes up that the brief does not cover, or that
  contradicts it, stop. Do not guess.** End your turn with a report whose
  first line is `QUESTION FOR THE USER`, followed by:
  - what you found, with file:line;
  - the options, each with what it changes;
  - your recommendation and a one-line reason;
  - the state you left the workspace in.
- When you finish, hand back the `implement` skill's report: what was
  built, the checks against your baseline, the review outcome, and any
  decision for the user.

### 4. Questions (the supervisor, with the user)

When a building agent reports `QUESTION FOR THE USER`:

1. Put the question to the user with `AskUserQuestion`, using the agent's
   options and recommendation. Do not answer it yourself, even when the
   answer looks obvious.
2. Amend that finding's brief with the answer and who made it, as #2's
   "Amendment: decision 5" did.
3. Resume the same agent with `SendMessage`, so it keeps its context. Give
   it the user's answer and the brief's new text.

Other agents in the wave keep running while one waits.

### 5. Merge (the supervisor, after the last wave)

The changes of one wave are siblings on the same base. At the end they
become one linear stack below `prwrkokz`:

1. **Order.** Use the order in "Waves" below: wave by wave, and within a
   wave in the order listed.
2. **Rebase** each change onto the one before it, for example
   `jj rebase -r <change> -o <previous change>`. jj records conflicts in
   the change instead of stopping.
3. **Resolve conflicts** change by change, from the bottom of the stack.
   For each conflicted change:
   - `jj edit` it in the main workspace (nothing in the stack has a
     bookmark yet, so the changes are not immutable);
   - resolve with the Edit tool;
   - run `just check` there;
   - jj rebases everything above it.

   A conflict whose resolution changes behaviour is a question for the
   user, as in step 4.
4. **Briefs.** The notes and the #4 brief are in `knyktlrn`. The rule
   "a brief lands in the same change as its code" still holds, so move
   each brief into its finding's change:
   `jj squash --from knyktlrn --into <change> <brief path>`. If
   `knyktlrn` is then empty except for this handoff, fold the handoff
   into the last change and abandon `knyktlrn`.
5. **Verify.** Run `just verify-stack` in the main workspace. Every
   revision must pass on its own, and the test count must rise up the
   stack.
6. **Clean up.** Run `jj workspace forget arch-NN` for each workspace, then
   delete its directory. Update this handoff's progress table, test counts
   and jj stack.

Never push without the user asking.

## Progress

| # | Finding | Status | Change | Brief |
|---|---|---|---|---|
| 1 | Report doesn't answer lifecycle questions ("failed (round 1)") | **Done**, reviewed | `vtruquzw` | `2026-10-20-arch-01-job-lifecycle.md` |
| 2 | A verdict is not the end of a round (lost `PullRequestOpened`) | **Done**, reviewed | `nnzttxxs` | `2026-10-20-arch-02-verdict-after-delivery.md` |
| 3 | Admission is coded twice | **Done**, reviewed | `smnununx` | `2026-10-22-arch-03-one-admission.md` |
| 4 | Round lifecycle split across run.rs/round.rs | **Built**, reviewed, merged (wave 1); decision 9 added mid-build | `lopouzot` | `2026-10-29-arch-04-round-owns-its-life.md` |
| 5 | A job's place under the root has no owner; RepoKey collision | **Built**, reviewed, merged (wave 1); spec layout updated (decision 10) | `ntqkpnqz` | `2026-10-29-arch-05-a-jobs-place.md` |
| 6 | "Where a job points" is plain strings; two "origin"s | **Built**, reviewed, merged (wave 3, 403 tests). Decisions 7–11 added mid-build: `pushed_to` is the URL; non-default ports in the key; clone-internal messages name the URL; every git call in the clone names the URL (`CLONE_REMOTE` gone), with a credential-helper test | `tylxnxll` | `2026-10-29-arch-06-remote-as-a-value.md` |
| 7 | The latest request is four Options | **Built**, reviewed, merged (wave 2). `for_round` gets the base twice (`requests.latest.base`, unused, and the admitted base, used for `--ref`); checked: no behaviour change | `uxmvumxr` | `2026-10-29-arch-07-the-latest-request.md` |
| 8 | One container boolean, four decisions | **Built**, reviewed, merged (wave 4, 411 tests). Decisions 5–7 done by the supervisor at the merge: the `credential_helper_for_this_environment` doc, `CLAUDE.md`'s Naming example, `for_round`'s doc | `wotpulqw` | `2026-10-29-arch-08-runner-requirements.md` |
| 9 | `run` says job where it means round | **Built**, reviewed, merged (wave 2); decisions 8–9 added mid-build (share `report.rs`'s line pieces, delete `to_conclusion_lines`, two doc fixes) | `rzuqykyv` | `2026-10-29-arch-09-run-speaks-of-rounds.md` |
| 10 | SHAs, refs and branches all `&str`; `REF@SHA` owned twice | **Built**, reviewed, merged (wave 4, 411 tests). The user accepted two readings: claim/submit keep `&str` for generic git refs; `RoundCommitted.sha` stays a `String` | `ztqqqqyp` | `2026-10-29-arch-10-typed-git-values.md` |
| 11 | Kubernetes reconnect policy tangled with I/O | **Built**, reviewed, merged (wave 1); decision 7 (held_for wiring test) added mid-build | `wplmtryt` | `2026-10-29-arch-11-kubernetes-reconnect-policy.md` |

Waves confirmed by the user on 2026-10-29. Wave 1 merged on 2026-10-29
with no text conflicts; checks clean at each change: `lopouzot` 380,
`ntqkpnqz` 387, `wplmtryt` 395 tests. The user left #4's unreachable
`(OpenPullRequest, None)` arm in `run.rs` as it is.

Wave 2 merged on 2026-10-30 in the order #7, #9, with no text conflicts:
`uxmvumxr` 397, `rzuqykyv` 398 tests; `just check` clean at the tip.

Wave 3 merged on 2026-10-30: `tylxnxll` 403 tests, `just check` clean.

#6 mid-build (2026-10-30), the user decided: `pushed_to` records the URL as
pushed, user info included (decision 7); a non-default port is kept in the
`RepoKey` as `host+PORT` (decision 8).

Known open, for a later item (found by #6's spelling table):
- A remote URL the person types with a password in it
  (`https://user:TOKEN@host/...`) is written into `RoundRequested.remote_url`,
  `BranchPushed.pushed_to` and a container launch's `--repo=`.
- IPv6 hosts are unkeyable; the daemon refuses them with a message.
- `run --repo=../bare` (a relative path to a bare repository) clones
  relative to the scratch directory, not the caller's directory.

Wave 4 merged on 2026-11-10 in the order #10, #8, with no text conflicts:
`ztqqqqyp` 411, `wotpulqw` 411 tests.

At the end, each brief was moved into its finding's change and this
handoff into `wotpulqw`; the planning change `knyktlrn` emptied and is
gone. All workspaces are forgotten and their directories deleted.

Lesson from the waves: supervisor edits go in a docs change on top of the
latest merged wave, never in a building agent's base, so no agent's
working copy goes stale while it works.

### Waves

The findings in one wave build at the same time. A finding goes in a later
wave when it needs an earlier finding's code, or when two findings would
rewrite the same lines. Each wave's base is the merged result of the waves
before it, so merge each wave before the next one starts (steps 2–3 of
"Merge", without the clean-up).

| Wave | Findings, in merge order | Base | Why |
|---|---|---|---|
| 1 | #4, #5, #11 | `knyktlrn` | Mostly disjoint files. #4: `round.rs`, `run.rs`, `delivery.rs`, `workspace.rs`. #5: `paths.rs`, `locate.rs`, `daemon/*`, `report.rs`. #11: `runner/kubernetes.rs` only. Expect a small conflict between #4 and #5 if #5 makes `JobReport.id` a `JobId` (`run.rs` folds frames with it) |
| 2 | #7, #9 (the user swapped the order on 2026-10-30, so #9 can fix `report.rs` on top of #7) | wave 1 merged | #9 renames the types #4 reshapes, so it needs #4. #7 changes the fold and fleet's key lookup, so it needs #5. Both may want the name `RoundRequest`: settle it in their grills, before the wave starts |
| 3 | #6 | wave 2 merged | It touches `run.rs`, `payload.rs` (`RoundPayload.remote_url`), `git.rs`, `paths.rs`, `locate.rs`, `submission.rs` and the daemon. Running it next to #9 or #7 would conflict on most of them |
| 4 | #10, #8 | wave 3 merged | #10 builds on #6's remote value. #8 shares the URL rewrite in `runner/mod.rs` with #6. #10 and #8 both touch `runner/mod.rs` (the `--ref` text and the rewrite), so expect a small conflict there |

The user has not confirmed these waves. Confirm them before the first
grill, and re-check the file lists after each grill: a grill can widen a
finding's scope and move it to another wave.

## What #1 settled that later findings must respect

- `JobReport.rounds` is the latest round's number:
  - each `RoundRequested` takes `rounds + 1`;
  - a `RoundStarted { round }` only raises it, which guards against a
    request lost to a torn log line (`read_events` skips any unparseable
    line, not just the last).
  - Never add one to it in a caller.
- `JobReport.any_round_started` is what "has output to show" keys on. Don't
  bring back `rounds > 0` for that.
- The user rejected a richer round-phase enum in favour of keeping
  `JobState`, `rounds` and `launched` as they are. Reconsider only if a
  finding adds a sixth state.
- Accepted risk: a cancel-then-revise log written before #1, whose revise is
  running when the daemon is upgraded, re-reads that round into both
  `assembly logs` and `events.jsonl`. The fold keeps the last of each event,
  so `status` is fine. This is already in the #1 brief.

## Remaining findings in brief

Each one lists the evidence from the report. Re-check it against the code
before grilling, because earlier findings move lines.

- **#3 Admission coded twice.**
  - `run::prepare_run` (`src/run.rs:160-173`) and `daemon::submit::accept`
    (`src/daemon/submit.rs:82-99`) repeat: pin the base, read the config at
    that SHA, resolve the provider, `reasons_it_cannot_run`, claim.
  - Nothing records that the checks passed, so later code checks again:
    `RoundPayload::for_round` repeats the provider lookup and the
    `max_duration` parse, and `RepoConfig::command_limit_secs` parses it a
    third time, lossily.
  - "No provider" travels as `""`.
  - A refusal in `run` goes to stderr before frames exist (`main.rs:117`), so
    the collector sees only "exit 2" (an F3 known-open item).
  - Direction: admission produces "the settings a job runs under", and the
    claim requires that value.
- **#4 Round lifecycle split.**
  - `run_round_in` returns a bare `Verdict`, so `branch_carries_work`
    (`run.rs:290`) rebuilds the tip from frames, and a git error there skips
    delivery silently (`unwrap_or(false)`).
  - The provisioning helpers live in `round.rs` but only `run.rs` uses them.
  - Direction: the round owns its whole life and ends in an outcome that
    carries its verdict and its work.
  - #2 takes the first step: the closing events come back to the caller.
- **#5 A job's place under the root.**
  - `JobPaths` and `dispatch::JobAddress` describe the same thing.
  - "Read the log, then fold" is written four times.
  - `JobReport.id: u64` should be a `JobId`.
  - `RepoKey::jobs_dir` has no terminator, so `/x/a.git`'s job 7 is
    `/x/a/7.git`'s jobs dir (an F3 known-open item).
  - `fleet::job_dirs_below` reverse-engineers the layout.
- **#6 Remote as a string.**
  - URL rules are spread over `payload.rs`, `git.rs`, `paths.rs` and
    `runner/mod.rs`.
  - "Checkout or URL?" is decided in three places.
  - `DEFAULT_REMOTE` is used inside the scratch clone, where
    `CLONE_REMOTE` is meant (`run.rs:183`).
  - `RoundPayload.remote_url` is dead.
- **#7 Latest request.** `remote_url`, `base`, `latest_prompt` and `provider`
  on `JobReport` are always set together, but callers re-check them
  (`dispatch.rs:400`, `submit.rs:168`).
- **#8 `RUNS_IN_A_CONTAINER`.** One boolean read in `daemon/mod.rs`,
  `submit.rs` and `runner/mod.rs` decides secrets, refusing local paths, the
  URL rewrite and `--provision-toolchain`.
- **#9 Language.** `run` says "job" where CLAUDE.md says "round"
  (`ReadyJob`, `JobConclusion`, `run_whole_job`, the `cli.rs:22` help).
  `payload::RoundRequest` sits next to the unrelated
  `EventKind::RoundRequested`. Best done with #3/#4.
- **#10 Stringly git.** `create_branch_if_absent(.., sha, branch)` and
  `check_out_new_branch(.., branch, at)` take their arguments in opposite
  orders. The `REF@SHA` form is written in `runner/mod.rs:75` and parsed in
  `run::split_pin`.
- **#11 Kubernetes reconnects.** The reconnect, drain or abandon decision is
  spread over `next_line` and `stream_after_follow_ended`
  (`kubernetes.rs:641-737`), so its tests use wall clocks.

## Practicalities

- **Checks:** `just check` (fmt-check, clippy `-D warnings`, test). It takes
  about 80s; `CARGO_TARGET_DIR` defaults to `/tmp/assembly-line-target`.
- **Test counts:** 361 at `main`, 367 after #1, 370 after #2, 373 after
  #3, 380 after #4, 387 after #5, 395 after #11, 397 after #7, 398 after
  #9, 403 after #6, 411 after #10, 411 after #8.
- **jj stack:** `main` → `vtruquzw` (#1) → `nnzttxxs` (#2) → `smnununx`
  (#3) → `lopouzot` (#4) → `ntqkpnqz` (#5) → `wplmtryt` (#11) →
  `uxmvumxr` (#7) → `rzuqykyv` (#9) → `tylxnxll` (#6) → `ztqqqqyp` (#10)
  → `wotpulqw` (#8) → `prwrkokz` (report HTML). Nothing is pushed. Never
  push without the user asking.
- **Edits:** Edit/Write tools only, never shell edits (see the user's
  global CLAUDE.md).
- **Review lesson from #1.** The reviewer caught a wrong claim in the brief's
  accepted-risk text, which had been put to the user that way. Before
  presenting a risk for acceptance, verify it with a throwaway run rather
  than by reading.

## What #3 settled that later findings must respect

- `admission::admit` is the one place a job is admitted, and `claim_job`
  takes its `Admitted`. The dispatcher admits again at launch for the
  round's settings. Don't read `RepoConfig` for a job's settings anywhere
  else.
- Each caller still pins its base its own way (scratch clone in `run`,
  bare cache in `submit`). Merging the pins is #6's ground.
- Under `--frames`, a refused `run` writes one `RoundFailed` frame, whose
  reason comes from `RunRefused::to_failure_reason`.

## Next action

All eleven findings are built, reviewed and in the stack. What is left is
the user's call:

1. Push and open a pull request, when the user asks.
2. The known-open items above (credentials in remote URLs, IPv6 hosts,
   `run --repo=../bare`), each as a new item if the user wants them.
