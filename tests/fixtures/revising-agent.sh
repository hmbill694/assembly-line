#!/usr/bin/env bash
# A fake agent that appends rather than overwrites.
#
# Round 2 can only produce a two-line file if it started from round 1's work,
# so this is what proves a revise round continues the job's branch instead of
# cutting a fresh one. $1 is the prompt.
set -euo pipefail
echo "revising-agent: $1"
printf '%s\n' "$1" >> rounds.txt
