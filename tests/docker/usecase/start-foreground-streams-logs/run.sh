#!/usr/bin/env bash
# USE CASE: `sysg start` WITHOUT --daemonize streams unit output to the console.
#
# WHAT THIS TESTS
#   A foreground start owns the terminal and must surface what its services print
#   — BOTH stdout and stderr — live on the console, tagged per unit. This is the
#   UX a user expects when running the stack in the foreground (like
#   docker-compose up). Streaming is TTY-gated, so the harness runs sysg under a
#   real PTY (fg.py); a plain redirect would suppress it, which is itself the
#   correct behavior for non-interactive callers.
#
# HARD INVARIANTS
#   - the console shows chatty's stdout line (OUT_MARKER_HELLO),
#   - the console shows chatty's stderr line (ERR_MARKER_OOPS) too,
#   - a SECOND unit's output also appears (multi-unit interleave),
#   - lines are tagged with their unit name ([chatty] / [quiet]),
#   - after Ctrl-C the stack is torn down (no leftover service processes).
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
OUT=/tmp/console.log

section "foreground start under a PTY, capture the console"
python3 /usecase/fg.py "$CONFIG" "$OUT" 8
check "$?" "foreground start ran and was torn down"
echo "----- captured console -----"
tr -d '\r' <"$OUT"
echo "----- end console -----"

section "unit stdout reached the console"
grep -q "OUT_MARKER_HELLO" "$OUT"
check "$?" "chatty's stdout line is on the console"

section "unit stderr ALSO reached the console"
grep -q "ERR_MARKER_OOPS" "$OUT"
check "$?" "chatty's stderr line is on the console (both streams surfaced)"

section "a second unit's output is interleaved in"
grep -q "SECOND_UNIT_LINE" "$OUT"
check "$?" "quiet's line is on the console too"

section "output is tagged per unit"
grep -q "\[chatty\]" "$OUT"
check "$?" "lines are prefixed with the unit name [chatty]"

section "the stack was torn down on Ctrl-C"
sleep 1
NOW="$(pgrep -x sleep | wc -l | tr -d ' ')"
echo "leftover sleep procs: $NOW (expected 0)"
[ "$NOW" = "0" ]
check "$?" "no service processes survive the foreground exit"

sysg purge --force >/dev/null 2>&1
finish
