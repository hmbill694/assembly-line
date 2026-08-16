# assembly-line task runner.
#
# Build output goes outside the repo so that per-revision verification of a jj
# stack never snapshots `target/` into a change (see CLAUDE.md).

export CARGO_TARGET_DIR := env_var_or_default("CARGO_TARGET_DIR", "/tmp/assembly-line-target")

default:
    @just --list

# Everything CI would run. The single gate before a change is considered done.
check: fmt-check lint test

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

# Run one graph end to end against a throwaway repo, to see real output.
demo:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --quiet
    bin="$CARGO_TARGET_DIR/debug/assembly"
    dir=$(mktemp -d)
    cd "$dir" && git init -q .
    cat > graph.toml <<'EOF'
    [[task]]
    id = "build"
    kind = "shell"
    run = "echo building; sleep 0.3"

    [[task]]
    id = "test-auth"
    kind = "shell"
    needs = ["build"]
    run = "echo testing auth; sleep 0.5"

    [[task]]
    id = "test-api"
    kind = "shell"
    needs = ["build"]
    run = "echo testing api; sleep 0.5"

    [[task]]
    id = "migrate"
    kind = "shell"
    needs = ["build"]
    run = "echo nope 1>&2; exit 3"

    [[task]]
    id = "deploy"
    kind = "shell"
    needs = ["migrate", "test-auth", "test-api"]
    run = "echo deploying"
    EOF
    "$bin" run graph.toml || true
    echo
    "$bin" status
    echo "demo run left in $dir"

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
    trap 'rm -rf "$work"' EXIT

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

# Prune build artifacts and the demo target directory.
clean:
    cargo clean || true
    rm -rf "$CARGO_TARGET_DIR"
