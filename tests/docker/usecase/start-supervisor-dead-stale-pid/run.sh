#!/usr/bin/env bash
# USE CASE: start after a supervisor crash left a stale sysg.pid behind.
#
# WHAT THIS TESTS
#   If a previous supervisor died without cleaning up, a stale sysg.pid (naming
#   a dead PID) is on disk. A fresh `sysg start` must detect the pid is dead,
#   recover, and boot normally -- NOT report "No running supervisor" and NOT
#   refuse to start. This is the SG0015 (state desynchronized) class that forced
#   users to `sysg purge` after every command.
#
# EXPECTED OUTCOME
#   - With a stale sysg.pid planted (pointing at a never-used PID), `sysg start`
#     exits 0, boots the supervisor, and `web` comes up running.
#   - No "No running supervisor" text is emitted to the terminal.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
STATE_DIR="$HOME/.local/share/systemg"

section "plant a stale supervisor pid (dead process)"
mkdir -p "$STATE_DIR"
# 999999 is not a live PID in this fresh container.
echo "999999" > "$STATE_DIR/sysg.pid"
[ -f "$STATE_DIR/sysg.pid" ]
check "$?" "stale sysg.pid planted"

section "start recovers from the stale pid and boots"
sysg start --config "$CONFIG" --daemonize 2>/tmp/stale_err.txt
RC=$?
cat /tmp/stale_err.txt
[ "$RC" = "0" ]
check "$?" "start exits 0 despite the stale pid"
if grep -qi "No running supervisor" /tmp/stale_err.txt; then
  check 1 "start did NOT emit 'No running supervisor'"
else
  check 0 "start did NOT emit 'No running supervisor'"
fi

sleep 3
STATUS="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
[ "$(unit_field "$STATUS" web state)" = "running" ]
check "$?" "web is running after recovery"
PID="$(unit_field "$STATUS" web pid)"
pid_alive "$PID"
check "$?" "web pid is alive per ps"

sysg stop --supervisor >/dev/null 2>&1
finish
