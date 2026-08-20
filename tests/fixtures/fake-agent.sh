#!/usr/bin/env bash
# A fake coding agent: writes a file named after its prompt and exits 0.
# Never touches the network. $1 is the prompt.
set -euo pipefail
echo "fake-agent: $1"
printf '%s\n' "$1" > agent-output.txt
