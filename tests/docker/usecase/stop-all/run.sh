#!/usr/bin/env bash
# USE CASE: `sysg stop` with no selector stops everything, keeps the supervisor.
#
# WHAT THIS TESTS
#   `sysg stop` with NO selector stops every service the resident supervisor
#   manages, but leaves the supervisor itself running. This is the "stop my
#   whole project, but don't tear down the daemon" workflow.
#
# EXPECTED OUTCOME
#   - After boot, svc1 and svc2 are both running with live pids.
#   - `sysg stop` (no selector) exits 0.
#   - Both svc1 and svc2 are no longer running; their recorded pids are dead.
#   - The supervisor is STILL up: `sysg status --format json` still exits 0.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot both services"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3

STATUS="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
PID1="$(unit_field "$STATUS" svc1 pid)"
PID2="$(unit_field "$STATUS" svc2 pid)"
echo "svc1 pid: $PID1  svc2 pid: $PID2"
[ "$(unit_field "$STATUS" svc1 state)" = "running" ]
check "$?" "svc1 is running before stop"
[ "$(unit_field "$STATUS" svc2 state)" = "running" ]
check "$?" "svc2 is running before stop"
pid_alive "$PID1"
check "$?" "svc1 pid is alive before stop"
pid_alive "$PID2"
check "$?" "svc2 pid is alive before stop"

section "stop with no selector stops every service"
sysg stop --config "$CONFIG"
check "$?" "stop (no selector) exits 0"
sleep 2

STATUS2="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
[ "$(unit_field "$STATUS2" svc1 state)" != "running" ]
check "$?" "svc1 is no longer running"
[ "$(unit_field "$STATUS2" svc2 state)" != "running" ]
check "$?" "svc2 is no longer running"
if pid_alive "$PID1"; then check 1 "svc1 pid is dead"; else check 0 "svc1 pid is dead"; fi
if pid_alive "$PID2"; then check 1 "svc2 pid is dead"; else check 0 "svc2 pid is dead"; fi

section "the supervisor is still up after stop-all"
check_ok "supervisor still running after stop-all" sysg status --format json

sysg stop --supervisor >/dev/null 2>&1
finish
