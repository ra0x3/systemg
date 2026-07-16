#!/usr/bin/env bash
# USE CASE: stop behaves correctly when the supervisor is alive but not serving.
#
# WHAT THIS TESTS
#   Two halves of BUG-4 on the stop path:
#     - `stop -s <svc>` into a frozen daemon must NOT hang or lie — it is refused
#       with a typed SG0205 (supervisor not responding), so a wedged daemon never
#       silently swallows a stop.
#     - `stop --supervisor` against a frozen daemon must still succeed: the daemon
#       is already going away, so we clear the stale runtime and report it down
#       rather than erroring on an undeliverable Shutdown.
#
# EXPECTED OUTCOME
#   - Boot demo; freeze the supervisor (SIGSTOP: alive pid, dead socket).
#   - `stop -s web`     -> non-zero, SG0205, does NOT hang.
#   - `stop --supervisor` -> exits 0, runtime cleared, supervisor reported down.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
STATE_DIR="$HOME/.local/share/systemg"

section "boot the config"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3
SUP1="$(cat "$STATE_DIR/sysg.pid")"
echo "supervisor pid: $SUP1"
[ -n "$SUP1" ] && pid_alive "$SUP1"
check "$?" "supervisor process alive"

section "freeze the supervisor (alive pid, dead socket)"
kill -STOP "$SUP1"
timeout 4 sysg status --config "$CONFIG" --format json >/dev/null 2>&1
[ "$?" = "124" ]
check "$?" "a status through the frozen socket TIMES OUT (not serving)"

section "stop -s web into a frozen daemon is refused with SG0205"
timeout 20 sysg stop --service web 2>/tmp/s.txt
RC=$?
cat /tmp/s.txt
[ "$RC" != "124" ]
check "$?" "stop -s web did NOT hang"
[ "$RC" != "0" ]
check "$?" "stop -s web exits non-zero"
stderr_has_code SG0205 /tmp/s.txt
check "$?" "stderr names SG0205 (supervisor not responding)"

section "stop --supervisor against a frozen daemon still succeeds"
timeout 20 sysg stop --supervisor 2>/tmp/sup.txt
RC=$?
cat /tmp/sup.txt
[ "$RC" != "124" ]
check "$?" "stop --supervisor did NOT hang"
[ "$RC" = "0" ]
check "$?" "stop --supervisor exits 0 (frozen daemon cleared)"

section "the runtime is cleared (pid file + socket removed)"
[ ! -e "$STATE_DIR/sysg.pid" ]
check "$?" "supervisor pid file removed by stop --supervisor"
[ ! -e "$STATE_DIR/control.sock" ]
check "$?" "control socket removed by stop --supervisor"

kill -CONT "$SUP1" 2>/dev/null
kill -9 "$SUP1" 2>/dev/null
finish
