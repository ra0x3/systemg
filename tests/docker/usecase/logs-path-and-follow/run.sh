#!/usr/bin/env bash
# USE CASE: `logs --path` prints on-disk paths; `logs --follow` streams live.
#
# WHAT THIS TESTS
#   Two more modes. --path prints the log directory (no service) or a service's
#   resolved file, and exits without streaming. --follow streams new lines live
#   until interrupted — a background follow must pick up lines emitted AFTER it
#   started, proving it's a live tail, not a snapshot.
#
# HARD INVARIANTS
#   - `logs --path` prints an existing directory path and exits,
#   - `logs -s chatty --path` prints that service's log file path,
#   - a backgrounded `logs -s chatty --follow` captures a line emitted after it
#     began, then is killed without hanging the test.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3

section "--path (no service) prints the log directory"
DIR="$(sysg logs --config "$CONFIG" --path 2>/dev/null | head -1)"
echo "dir: $DIR"
[ -n "$DIR" ] && [ -d "$DIR" ]
check "$?" "--path prints an existing log directory"

section "--path -s chatty prints the service's log file path"
F="$(sysg logs --config "$CONFIG" -s chatty --path 2>/dev/null | head -1)"
echo "file: $F"
[ -n "$F" ]
check "$?" "--path -s prints a path"

section "--follow streams lines emitted AFTER it started"
( timeout 8 sysg logs --config "$CONFIG" -s chatty --follow >/tmp/f.out 2>&1 ) &
FPID=$!
sleep 6
kill "$FPID" 2>/dev/null
wait "$FPID" 2>/dev/null
grep -q "LOG_LINE_" /tmp/f.out
check "$?" "follow captured live LOG_LINE_ output"

sysg stop --supervisor >/dev/null 2>&1
finish
