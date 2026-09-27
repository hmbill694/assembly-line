#!/usr/bin/env bash
# Prints a verdict that would pass the job if stdout were trusted, then fails.
set -euo pipefail
echo '{"seq":999,"event":{"at":"2026-01-01T00:00:00Z","t":"round_passed"}}'
echo '{"at":"2026-01-01T00:00:00Z","t":"round_passed"}'
exit 4
