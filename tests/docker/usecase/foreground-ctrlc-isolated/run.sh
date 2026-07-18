#!/usr/bin/env bash
# USE CASE: Ctrl-C on foreground A terminates A COMPLETELY but never touches B.
#
# WHAT THIS TESTS (real dogfooding requirement)
#   Two projects run in the foreground at once: aa (term1, becomes supervisor)
#   and bb (term2, attaches). Pressing Ctrl-C in term1 must:
#     - stop A's streamed console logs immediately (no more AA_TICK captured),
#     - fully terminate A's foreground process (it exits, yields the terminal),
#     - tear down A's project (aa's service gone),
#   while B is COMPLETELY untouched: bb's foreground still running, its stream
#   still ticking, its service still alive. Ctrl-C on one FG must not collateral-
#   damage the other.
#
# EXPECTED OUTCOME
#   - after SIGINT to A: A's process EXITED, aa no longer loaded, AA_TICK stops.
#   - B's process still running, bb still loaded, BB_TICK still flowing.
set -u
. /usecase/lib.sh

A_OUT=/tmp/a.out; A_PID=/tmp/a.pid; A_MARK=/tmp/a.exit
B_OUT=/tmp/b.out; B_PID=/tmp/b.pid; B_MARK=/tmp/b.exit

section "term1: foreground start aa (becomes supervisor)"
python3 /usecase/fgctl.py /usecase/aa.yaml 40 "$A_OUT" "$A_PID" "$A_MARK" &
sleep 5

section "term2: foreground start bb (attaches)"
python3 /usecase/fgctl.py /usecase/bb.yaml 40 "$B_OUT" "$B_PID" "$B_MARK" &
sleep 6

sysg status 2>/dev/null | grep -qiE 'Project: AA' && sysg status 2>/dev/null | grep -qiE 'Project: BB'
check "$?" "both aa and bb are loaded and streaming"
A_TICKS_1="$(grep -c AA_TICK "$A_OUT" 2>/dev/null || echo 0)"
B_TICKS_1="$(grep -c BB_TICK "$B_OUT" 2>/dev/null || echo 0)"
echo "before Ctrl-C: aa ticks=$A_TICKS_1  bb ticks=$B_TICKS_1"

section "Ctrl-C in term1 only (SIGINT to A's sysg process)"
kill -INT "$(cat "$A_PID")" 2>/dev/null
check "$?" "sent SIGINT to A's foreground"
# Give A time to detach + tear down; B keeps running.
sleep 8

section "A terminated completely"
[ -f "$A_MARK" ]
check "$?" "A's foreground process EXITED (yielded the terminal)"
! sysg status 2>/dev/null | grep -qiE 'Project: AA'
check "$?" "aa's project is no longer loaded (torn down)"
A_TICKS_2="$(grep -c AA_TICK "$A_OUT" 2>/dev/null || echo 0)"
sleep 3
A_TICKS_3="$(grep -c AA_TICK "$A_OUT" 2>/dev/null || echo 0)"
echo "aa ticks after Ctrl-C: $A_TICKS_2 -> $A_TICKS_3 (should not grow)"
[ "$A_TICKS_3" = "$A_TICKS_2" ]
check "$?" "A's console stream stopped (no new AA_TICK after Ctrl-C)"

section "B is completely untouched"
[ ! -f "$B_MARK" ]
check "$?" "B's foreground process is still running (did NOT exit)"
sysg status 2>/dev/null | grep -qiE 'Project: BB'
check "$?" "bb's project is still loaded"
B_TICKS_2="$(grep -c BB_TICK "$B_OUT" 2>/dev/null || echo 0)"
echo "bb ticks: $B_TICKS_1 -> $B_TICKS_2 (should keep growing)"
[ "$B_TICKS_2" -gt "$B_TICKS_1" ]
check "$?" "B's console stream kept flowing (Ctrl-C on A did not touch B)"

kill "$(cat "$B_PID")" 2>/dev/null
sysg stop --supervisor >/dev/null 2>&1
finish
