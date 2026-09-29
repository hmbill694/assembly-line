#!/usr/bin/env bash
# Waits until the file $GATE exists, so a test decides when the round ends.
set -euo pipefail
echo "gated-agent: $1"
for _ in $(seq 1 600); do [ -e "$GATE" ] && break; sleep 0.05; done
printf '%s\n' "$1" > agent-output.txt
