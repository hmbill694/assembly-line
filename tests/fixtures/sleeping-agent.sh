#!/usr/bin/env bash
# Runs far longer than any test waits, so only cancellation can end it.
set -euo pipefail
echo "sleeping-agent: $1"
sleep 60
