#!/usr/bin/env bash
# Writes a partial change, then fails — the worktree must be kept.
set -euo pipefail
echo "fake-agent: starting work on $1"
printf 'half done\n' > partial.txt
echo "fake-agent: giving up" 1>&2
exit 3
