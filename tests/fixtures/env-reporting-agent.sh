#!/usr/bin/env bash
# A fake coding agent that reports whether the job's own variables reached
# it, then exits 0 having changed nothing. Never touches the network.
set -euo pipefail
echo "token=${ASSEMBLY_GIT_TOKEN:-absent} payload=${ASSEMBLY_JOB:-absent}"
