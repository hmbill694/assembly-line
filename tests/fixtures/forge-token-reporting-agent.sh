#!/usr/bin/env bash
# A fake coding agent that records whether the forge token reached it.
set -euo pipefail
echo "${GH_TOKEN:-absent}" > forge-token.txt
