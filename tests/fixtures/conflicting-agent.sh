#!/usr/bin/env bash
# Every instance writes the same file with different content, so the second
# merge into the run branch conflicts. $2 distinguishes them.
#
# The pause is what makes that deterministic: both instances must branch from
# the same run-branch tip before either of them merges. Without it, a fast
# first node could merge before the second even read the tip, and the second
# would then rebase cleanly onto it and never conflict.
set -euo pipefail
sleep 0.3
printf 'written by %s\n' "${2:-unknown}" > shared.txt
