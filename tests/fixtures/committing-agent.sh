#!/usr/bin/env bash
# Commits its own work and leaves the tree clean, as agents that
# auto-commit do. $1 is the prompt.
set -euo pipefail
printf '%s\n' "$1" > agent-output.txt
git add agent-output.txt
git commit --quiet --no-verify -m "agent: $1"
