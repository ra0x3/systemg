#!/usr/bin/env bash
# USE CASE: `sysg stop --supervisor` tears the whole daemon down cleanly.
#
# WHAT THIS TESTS
#   `sysg stop --supervisor` shuts the whole daemon down and clears its socket +
#   pid file, so a later status reports no supervisor (not a stale one). A
#   half-torn-down daemon that leaves its socket behind is the classic "sysg
#   thinks it's up but isn't" wedge.
#
# EXPECTED OUTCOME
#   - After boot, svc1 is running.
#   - `sysg stop --supervisor` exits 0.
#   - The control socket is gone.
#   - sysg.pid is gone.
#   - A subsequent plain `sysg status` FAILS and says "No running supervisor".
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
STATE_DIR="$HOME/.local/share/systemg"

section "boot the daemon"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3

STATUS="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
[ "$(unit_field "$STATUS" svc1 state)" = "running" ]
check "$?" "svc1 is running before supervisor stop"

section "stop --supervisor tears the daemon down"
sysg stop --supervisor
check "$?" "stop --supervisor exits 0"
sleep 2

[ ! -S "$STATE_DIR/control.sock" ]
check "$?" "control socket is gone"
[ ! -f "$STATE_DIR/sysg.pid" ]
check "$?" "sysg.pid is gone"

section "a later status reports no supervisor, not a stale one"
if sysg status 2>/tmp/s.txt; then
  check 1 "status fails when no supervisor is running"
else
  check 0 "status fails when no supervisor is running"
fi
grep -qi "No running supervisor" /tmp/s.txt
check "$?" "status says 'No running supervisor'"

finish
