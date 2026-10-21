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

**Grill one, build it, then move on.** One finding at a time, in report
order:

1. **Grill.** Use the `grill-me` skill. One question per message, each with a
   recommended answer and a one-line reason.
   - Settle the outcome and the definition of done before design, and design
     before risks.
   - Look facts up in the code yourself; put decisions to the user.
   - Finish with a teach-back and a brief saved as
     `docs/superpowers/plans/<date>-arch-NN-<slug>.md`, and get explicit
     approval before building.
2. **Build.** Use the `implement` skill on the approved brief:
   - baseline first;
   - tests first;
   - the de-ai standard on added lines;
   - an adversarial review in a fresh sub-agent that gets only the range,
     the brief and the baseline;
   - fix its findings, re-check, and squash the fixes into the change.
3. **One jj change per finding**, inserted above the previous finding's
   change and below `prwrkokz`. The brief lands in the same change as its
   code. The commit body records the test count.
4. **Hand back.** What changed, the checks, the review outcome, and any
   decision for the user. Then start grilling the next finding.

When something comes up mid-build that the brief doesn't cover, stop and ask
the user, then amend the brief. This happened in #1 and was the right call.

## Progress

| # | Finding | Status | Change | Brief |
|---|---|---|---|---|
| 1 | Report doesn't answer lifecycle questions ("failed (round 1)") | **Done**, reviewed | `vtruquzw` | `2026-10-20-arch-01-job-lifecycle.md` |
| 2 | A verdict is not the end of a round (lost `PullRequestOpened`) | **Done**, reviewed | `nnzttxxs` | `2026-10-20-arch-02-verdict-after-delivery.md` |
| 3 | Admission is coded twice | Not started | — | — |
| 4 | Round lifecycle split across run.rs/round.rs | Not started; #2 does part of it | — | — |
| 5 | A job's place under the root has no owner; RepoKey collision | Not started | — | — |
| 6 | "Where a job points" is plain strings; two "origin"s | Not started | — | — |
| 7 | The latest request is four Options | Not started | — | — |
| 8 | One container boolean, four decisions | Not started | — | — |
| 9 | `run` says job where it means round | Not started | — | — |
| 10 | SHAs, refs and branches all `&str`; `REF@SHA` owned twice | Not started | — | — |
| 11 | Kubernetes reconnect policy tangled with I/O | Not started | — | — |

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
- **Test counts:** 361 at `main`, 367 after #1, 370 after #2.
- **jj stack:** `main` → `vtruquzw` (#1) → `nnzttxxs` (#2) → `prwrkokz`
  (report HTML). Nothing is pushed. Never push without the user asking.
- **Edits:** Edit/Write tools only, never shell edits (see the user's
  global CLAUDE.md).
- **Review lesson from #1.** The reviewer caught a wrong claim in the brief's
  accepted-risk text, which had been put to the user that way. Before
  presenting a risk for acceptance, verify it with a throwaway run rather
  than by reading.

## Next action

Grill #3. Its brief and code go in a new change inserted above `nnzttxxs`
and below `prwrkokz`.
