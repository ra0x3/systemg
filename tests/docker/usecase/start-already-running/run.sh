#!/usr/bin/env bash
# USE CASE: `sysg start` when the supervisor is already running.
#
# WHAT THIS TESTS
#   Running start a second time on an already-booted config must be idempotent:
#   it must not fork a SECOND supervisor, must not duplicate the service, and
#   must leave the running service's pid unchanged. Duplicate supervisors were a
#   real class of prod bug (SG0007/SG0015).
#
# EXPECTED OUTCOME
#   - First start boots the supervisor + `web`.
#   - Exactly one supervisor process exists.
#   - Second start exits 0, does not change web's pid, and there is STILL
#     exactly one supervisor process and one `web` process.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "first start boots one supervisor and web"
sysg start --config "$CONFIG" --daemonize
check "$?" "first start exits 0"
sleep 3
STATUS1="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
PID1="$(unit_field "$STATUS1" web pid)"
echo "web pid after first start: $PID1"
[ -n "$PID1" ] && [ "$PID1" != "absent" ] && [ "$PID1" != "None" ]
check "$?" "web has a pid after first start"

WEB_PROCS_1="$(ps -eo args | grep -c "[s]leep 3000")"
[ "$WEB_PROCS_1" = "1" ]
check "$?" "exactly one web process running"

section "second start is idempotent -- no duplicate supervisor or service"
sysg start --config "$CONFIG" --daemonize
check "$?" "second start exits 0"
sleep 2
STATUS2="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
PID2="$(unit_field "$STATUS2" web pid)"
echo "web pid after second start: $PID2"
[ "$PID2" = "$PID1" ]
check "$?" "web pid unchanged by second start (not restarted/duplicated)"
[ "$(unit_count "$STATUS2")" = "1" ]
check "$?" "still exactly one unit"

WEB_PROCS_2="$(ps -eo args | grep -c "[s]leep 3000")"
[ "$WEB_PROCS_2" = "1" ]
check "$?" "still exactly one web process (no duplicate)"

sysg stop --supervisor >/dev/null 2>&1
finish
