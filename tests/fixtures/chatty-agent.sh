#!/usr/bin/env bash
# Says who it is, then keeps talking for far longer than any test waits, so
# only cancellation can end it — and every line is a write the collector
# must land.
set -euo pipefail
echo "chatty-agent pid=$$"
for _ in $(seq 600); do
  echo "chatty-agent: still going"
  sleep 0.1
done
