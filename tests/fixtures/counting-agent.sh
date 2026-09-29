#!/usr/bin/env bash
# Records how many copies of itself are live at once, in $PROBE_DIR, so a
# test can prove a concurrency cap by observation rather than by trusting
# the scheduler's own counters. $1 is the prompt.
set -euo pipefail
mkdir -p "$PROBE_DIR"
touch "$PROBE_DIR/live.$$"
ls "$PROBE_DIR" | grep -c '^live\.' >> "$PROBE_DIR/seen" || true
# Long enough that rounds dispatched together overlap even on a busy machine.
sleep 2
rm "$PROBE_DIR/live.$$"
printf '%s\n' "$1" > agent-output.txt
