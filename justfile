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

# Prune build artifacts and the out-of-repo target directory.
clean:
    cargo clean || true
    rm -rf "$CARGO_TARGET_DIR"
