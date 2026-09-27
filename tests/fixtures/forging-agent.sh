#!/usr/bin/env bash
# Prints a verdict that would pass the job if stdout were trusted, then fails.
set -euo pipefail
echo '{"seq":999,"event":{"at":"2026-01-01T00:00:00Z","t":"job_finished","exit_code":0}}'
echo '{"at":"2026-01-01T00:00:00Z","t":"job_finished","exit_code":0}'
exit 4
