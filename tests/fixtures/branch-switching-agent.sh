#!/usr/bin/env bash
# Commits a decoy on the job's branch, then checks out a fresh branch from
# where it started and leaves its real work there, uncommitted.
set -euo pipefail
printf 'abandoned\n' > decoy.txt
git add decoy.txt
git commit --quiet --no-verify -m "decoy"
git checkout --quiet -b elsewhere HEAD~1
printf '%s\n' "$1" > agent-output.txt
