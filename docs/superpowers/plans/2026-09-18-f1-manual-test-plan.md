# F1 Manual Test Plan — assembly-line against its own repo

**For:** a human driving the real binary by hand, against this repository.
**Milestone:** F1 (`docs/superpowers/plans/2026-09-11-software-factory-f1.md`).
**Automated coverage:** 165 tests, all with shell-script fake agents. This plan
exercises what they cannot — a **real** coding agent, a **real** repository
with history, and the jj-colocated setup this repo actually uses.

Every step says what to run, what you should see, and what it would mean if
you saw something else.

---

## Before you start: read this section

### Two things will bite you immediately

**1. This repo cannot opt itself in without an edit.** `.assembly/` is ignored
in *both* `.gitignore` and `.git/info/exclude`, but `RepoConfig::from_ref`
reads `.assembly/config.toml` **out of a committed ref**, never from your
working tree — that is the property that stops a job editing the settings that
govern it. An ignored, uncommitted config is invisible to the tool, and you
will get:

```
HEAD carries no .assembly/config.toml — this repository is not opted in
```

This is a real F1 defect, not your mistake — F1 left `.assembly/` meaning both
*tracked config* and *ignored job state*. The spec says these must never share
a directory, and F3 moves job state out. Step 1 works around it.

**2. This repo is jj-colocated, and `assembly` drives plain git.** It creates
`git worktree`s and `al/job-N` branches through git subprocesses. jj will
import those refs as it notices them. Nothing here writes to your working copy
or moves a bookmark, but expect `jj log` to start showing `al/job-*` refs.
**Do the whole plan on a scratch clone if you would rather not find out.**

### The safety properties you are checking as you go

- The target repo's **working tree is never modified**. `git status` clean
  before and after, every time.
- A job's **worktree is scratch** and always removed — success or failure.
- A job's **branch always survives** — success or failure. That is the whole
  point of the milestone.
- Job state lives under `<git-root>/.assembly/jobs/<id>/`; worktrees live
  under `~/.assembly/wt/`, **never inside the repo**.

### Setup

```bash
cd /Users/harry-mac/code-stuff/assembly-line
cargo build                       # or: just build
export AL="$PWD/target/debug/assembly"
# If you use the justfile's target dir instead:
# export AL="/tmp/assembly-line-target/debug/assembly"
$AL --version
```

Keep a second terminal open on `watch -n1 'git status --short'` if you want to
*see* the "never modifies your tree" property hold rather than take my word.

---

## Step 1 — Opt the repository in

The config must be **committed**, so the ignore rules have to stop swallowing
it.

Narrow both ignore files from `.assembly/` to `.assembly/jobs/`:

- `.gitignore` — change the `.assembly/` line to `.assembly/jobs/`
- `.git/info/exclude` — same change

Then write `.assembly/config.toml`:

```toml
provider = "claude"
verify = "cargo build --quiet"

[providers.claude]
cmd = "claude"
args = ["-p", "{prompt}", "--permission-mode", "acceptEdits"]
```

`verify = "cargo build --quiet"` is deliberately cheap — `cargo test` on this
repo takes long enough to make the loop tedious. You will tighten it in Step 6.

Commit it (jj snapshots the working copy, so `jj describe` is enough):

```bash
jj describe -m "chore: opt this repo into the factory"
jj new
```

**Confirm the tool can see it** — this reads from the ref, not your tree:

```bash
git show @-:.assembly/config.toml
```

**Expected:** the file's contents.
**If it errors:** the config is not committed. Nothing below will work until
this prints.

---

## Step 2 — The smallest real job

```bash
$AL run --prompt "Add a doc comment to the parse_duration function in src/config.rs explaining what duration formats it accepts. Change nothing else."
```

**Expected:**
- A real `claude` session runs; its output streams to a log, not your terminal.
- `job 1: succeeded (round 1, 1 file +N/-M)`
- `branch: al/job-1`
- A delivery line. This repo has no `origin`, so expect
  `not delivered: the repository has no 'origin' remote, so the branch stays local`
- `state: .../.assembly/jobs/1`

**Then check the four invariants:**

```bash
git status --short                    # must be clean — your tree was not touched
git log --oneline al/job-1 -1         # the branch exists and carries a commit
git diff HEAD al/job-1 -- src/config.rs   # the agent's actual change
ls ~/.assembly/wt/*/                  # the worktree must be GONE
```

**What would be wrong:**
- A dirty working tree → the "target repo is never modified" invariant is
  broken. Stop and report it.
- A surviving worktree directory → "the worktree is scratch" is broken.
- No `al/job-1` branch → the central invariant is broken.

---

## Step 3 — Read the job back

```bash
$AL status
$AL status 1
$AL logs 1 | tail -40
```

**Expected:** `status` shows state, round, duration, diff and branch for job 1.
`logs` shows the agent's own output, and — because `verify` ran in the same
checkout — the `cargo build` output after it.

**Worth noticing:** `status` with no id defaults to the most recent job.

---

## Step 4 — Revise it

```bash
$AL revise 1 "Also mention in the comment that an invalid duration is reported with the offending text."
```

**Expected:**
- `revising job 1 (round 2)`
- The agent sees its **own previous work as files on disk** — no conversation
  replay, no session id. Check the log: it should be editing, not starting over.
- `job 1: succeeded (round 2, ...)`
- The round **appends to the same branch**:

```bash
git log --oneline al/job-1        # should now show TWO commits
```

**What would be wrong:** a second branch, or round 2's commit replacing round
1's. Rounds accumulate; nothing is rewritten.

---

## Step 5 — Watch a job fail, and confirm its branch survives

This is the invariant the whole milestone rests on: **a failure is a branch,
not a lost afternoon.**

```bash
$AL run --prompt "Add a function named deliberately_broken to src/config.rs whose body is exactly: this is not valid rust"
```

**Expected:**
- The agent does it; `verify` (`cargo build`) rejects the work.
- `job 2: failed (round 1, ...) — verify rejected the work: exit 101`
- `branch: al/job-2 (not delivered — the job did not pass)` — the branch name
  is printed *instead of* a pull request.
- Exit code 1.

**Then confirm the failure is inspectable:**

```bash
echo $?                          # 1
git log --oneline al/job-2 -1    # the branch EXISTS with the broken work on it
git diff HEAD al/job-2           # you can read exactly what it did
ls ~/.assembly/wt/*/             # still no worktree
git status --short               # still clean
```

**This is the single most important check in the plan.** A failed job that
leaves no branch, or leaves a worktree, means F1 did not deliver its premise.

---

## Step 6 — Prove `verify` is real, and reads the *committed* tree

`verify` was parsed and ignored for three milestones. Prove it now bites.

Edit `.assembly/config.toml` to a verify that can only pass if it runs **after
the commit, in the job's own checkout**:

```toml
verify = "git show --name-only --format= HEAD | grep -q src/"
```

Commit it (`jj describe -m "..." && jj new`), then:

```bash
$AL run --prompt "Add a blank line at the end of src/lib.rs"
```

**Expected:** succeeds — the agent touched a file under `src/`, and `verify`
can see it in `HEAD` because the commit already happened.

Now the negative case:

```bash
$AL run --prompt "Create a file called notes.txt at the repo root containing the word hello. Do not touch anything under src/."
```

**Expected:** **fails** — the commit exists, but nothing under `src/` is in it.

Together these prove `verify` runs *after* the commit and *inside* the job's
checkout. If the second one passes, `verify` is running in the wrong place or
at the wrong time.

---

## Step 7 — Config comes from the ref, not your working tree

The security property. A job must not be able to rewrite the rules that govern
it.

**Without committing**, edit `.assembly/config.toml` in your working tree to
something obviously wrong — point the provider at a command that does not
exist:

```toml
provider = "bogus"

[providers.bogus]
cmd = "definitely-not-a-real-command"
args = []
```

Then run a job:

```bash
$AL run --prompt "Add a blank line to the end of README.md"
```

**Expected:** the job runs with the **committed** `claude` provider and
succeeds. Your uncommitted edit is ignored entirely.

**If it tries to run `definitely-not-a-real-command`,** config is being read
from the checkout rather than the ref, and the "a job cannot edit its own
settings" property is false. That is a serious finding — report it.

Revert your working-tree edit afterwards (`jj restore .assembly/config.toml`,
or just undo it by hand).

---

## Step 8 — `--ref` branches from somewhere else

```bash
$AL run --ref main --prompt "Add a blank line to the end of README.md"
git log --oneline -1 al/job-N^      # N = the id just printed
```

**Expected:** the new branch's parent is `main`'s tip, not your current
working-copy commit.

---

## Step 9 — Cleanup, and leaving no trace

```bash
$AL gc --dry-run      # should find nothing: jobs discard their own worktrees
$AL gc
```

**Expected:** `nothing to collect`. `gc` exists for jobs that died *mid-run*;
a clean run leaves it nothing to do.

**Tear down what the plan created:**

```bash
git branch -D $(git branch --list 'al/job-*')    # the job branches
rm -rf .assembly/jobs                            # job state
# revert the .gitignore / .git/info/exclude edits from Step 1
# abandon or keep the config commits as you prefer
```

---

## What this plan does NOT cover, and why

| Not covered | Why |
|---|---|
| Delivery opening a real pull request | This repo has no `origin`. To test it, push a clone to a throwaway GitHub repo and set `[delivery] mode = "pr"`. `gh` must be installed and authenticated. |
| `--repo` against another repository | Worth trying if you use it; note the read commands (`status`, `logs`, `revise`) need the **same** `--repo` or they will not find the job. |
| `max_duration`, `copy` | Parsed and wired, but exercised only by the automated suite. |
| Concurrency | F1 runs exactly one job per invocation. Concurrency arrives with the watcher in F4. |

---

## Findings worth reporting back

Note anything here, since these are what a second pair of hands is for:

1. **The `.assembly/` ignore collision** (Step 1) — you will hit it before
   anything else works. Already known; F3 fixes it by moving job state out of
   the repo. Confirm it is as annoying as it sounds.
2. **jj + git worktrees** — whether `git worktree add` inside a jj-colocated
   repo causes jj any confusion, and whether `al/job-*` refs appearing in
   `jj log` is tolerable or noisy.
3. **Whether the log is readable.** `logs <id>` concatenates the agent's output
   and `verify`'s output into one file with nothing marking the boundary. If
   that is hard to read in practice, it is worth a fix.
4. **Whether a real agent's failure modes match the fakes.** Every automated
   test uses a shell script that fails deterministically. A real agent that
   stalls, asks a question, or exits 0 having done nothing is the interesting
   case, and nothing automated covers it.
