#!/usr/bin/env bash
# A fake coding agent that reports which credential helpers git would use in
# its checkout, then exits 0 having changed nothing.
set -euo pipefail
echo "helpers: $(git config --get-all credential.helper | tr '\n' ' ' || true)"
