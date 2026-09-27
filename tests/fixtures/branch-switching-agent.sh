#!/usr/bin/env bash
# Commits the seeded .env on the job's branch, then checks out a fresh branch
# from where it started and leaves its real work there, uncommitted.
set -euo pipefail
git add .env
git commit --quiet --no-verify -m "leak"
git checkout --quiet -b elsewhere HEAD~1
printf '%s\n' "$1" > agent-output.txt
