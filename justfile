# assembly-line task runner.
#
# Build output goes outside the repo so that per-revision verification of a jj
# stack never snapshots `target/` into a change (see CLAUDE.md).

set shell := ["bash", "-euo", "pipefail", "-c"]

export CARGO_TARGET_DIR := env_var_or_default("CARGO_TARGET_DIR", "/tmp/assembly-line-target")

default:
    @just --list

# Exactly what CI's Check job runs, in the same order — one step per
# dependency there, so a failure names itself. Keep the two lists identical.
ci: fmt-check lint test

# The single gate before a change is considered done.
check: ci

# Known vulnerabilities in the locked dependencies. Advisory in CI: an
# advisory with no fix yet should not block every PR.
audit:
    cargo audit

build:
    cargo build

test:
    cargo test

# Pedantic is configured in Cargo.toml, so this matches what your editor shows.
lint:
    cargo clippy --all-targets -- -D warnings

fmt:
    cargo fmt

fmt-check:
    cargo fmt --check

# Explain every pedantic finding rather than just listing them.
lint-explain:
    cargo clippy --all-targets --message-format=short 2>&1 | grep -E "warning|error" || echo "clean"

# Check every change in the jj stack independently, oldest first.
#
# Each revision is exported to a temp directory and built there, so this never
# touches your working copy and works even when part of the stack is immutable
# (which it is as soon as a bookmark points into it).
verify-stack:
    #!/usr/bin/env bash
    set -uo pipefail
    failed=0
    work=$(mktemp -d)
    # The last revision built leaves artifacts in the shared target dir that
    # were compiled from `$work`, which is about to vanish — and `env!(
    # "CARGO_MANIFEST_DIR")` is baked in at compile time. A later `just test`
    # would reuse them and every test that resolves a fixture path would fail
    # with a mystifying "no such file". Drop them on the way out.
    trap 'rm -rf "$work"; cargo clean -p assembly-line --quiet 2>/dev/null || true' EXIT

    while IFS=' ' read -r sha rev desc; do
      [ -n "$sha" ] || continue
      tree="$work/$rev"
      mkdir -p "$tree"
      git archive "$sha" | tar -x -C "$tree"

      if [ ! -f "$tree/Cargo.toml" ]; then
        printf '%-9s %-58s (no crate yet)\n' "$rev" "${desc:0:56}"
        continue
      fi

      # Different source trees share one target dir, and cargo will happily
      # reuse the previous revision's `assembly-line` artifacts — which makes
      # a correct revision look broken. Dropping just our package forces a
      # rebuild while keeping the expensive dependency artifacts.
      (cd "$tree" && cargo clean -p assembly-line --quiet 2>/dev/null) || true

      # cargo directly, not `just check`: the justfile does not exist at
      # revisions below the one that introduced it.
      result="ok"
      for step in "fmt --check" "clippy --all-targets -- -D warnings" "test"; do
        # shellcheck disable=SC2086
        if ! (cd "$tree" && cargo $step) > "$tree/.check.log" 2>&1; then
          result="FAILED at cargo $step"
          failed=1
          break
        fi
      done
      printf '%-9s %-58s %s\n' "$rev" "${desc:0:56}" "$result"
      if [ "$result" != "ok" ]; then
        sed 's/^/           | /' "$tree/.check.log" | head -12
      fi
    done < <(jj log --no-graph --reversed -r '::@ ~ root()' \
               -T 'commit_id.short() ++ " " ++ change_id.shortest(8) ++ " " ++ description.first_line() ++ "\n"')

    exit $failed

# Submit one job to a daemon and watch it end to end, to see real output.
#
# The repo, its origin, the daemon's root and the job state are all under one
# throwaway directory — this never touches the repo you're standing in or your
# real state root. Delivery is off, so your `gh` is never asked for anything.
demo:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --quiet
    bin="$CARGO_TARGET_DIR/debug/assembly"
    fake="$PWD/tests/fixtures/fake-agent.sh"
    dir=$(mktemp -d)
    export ASSEMBLY_ROOT="$dir/root"
    git init -q --bare "$dir/origin.git"
    git init -q --initial-branch=main "$dir/repo"
    cd "$dir/repo"
    git config user.email t@e.com && git config user.name T
    git config commit.gpgsign false
    git remote add origin "$dir/origin.git"
    mkdir -p .assembly
    cat > .assembly/config.toml <<EOF
    provider = "fake"
    verify = "git show --name-only --format= HEAD | grep -q agent-output.txt"

    [providers.fake]
    cmd = "bash"
    args = ["$fake", "{prompt}", "demo"]

    [delivery]
    mode = "none"
    EOF
    git add -A && git commit -qm "opt in to the factory"
    git push -q origin main
    "$bin" daemon &
    daemon=$!
    trap 'kill "$daemon" 2>/dev/null || true' EXIT
    until [ -S "$ASSEMBLY_ROOT/daemon.sock" ]; do
      kill -0 "$daemon" || exit 1
      sleep 0.1
    done
    "$bin" submit --prompt "make a change"
    # Captured before grep: `grep -q` can close the pipe while `status` is
    # still printing, and pipefail would read that as no verdict yet.
    until status=$("$bin" status) && grep -qE '^job [0-9]+: (passed|failed)' <<<"$status"; do
      kill -0 "$daemon" || exit 1
      sleep 0.5
    done
    "$bin" status
    echo
    "$bin" logs 1
    echo "demo job left in $dir (its branch is on $dir/origin.git)"

# Build the job image locally, for the host's platform.
image tag="assembly-line:dev":
    docker buildx build --load -t {{ tag }} .

# Boot the built image against a scratch repository and prove `assembly run`
# runs a job end to end. Needs a docker daemon; not part of `just check`.
smoke-docker tag="assembly-line:dev": (image tag)
    scripts/smoke-docker.sh {{ tag }}

# Prune build artifacts and the out-of-repo target directory.
clean:
    cargo clean || true
    rm -rf "$CARGO_TARGET_DIR"
