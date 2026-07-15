#!/usr/bin/env bash
# USE CASE: stopping one service actually kills its process.
#
# WHAT THIS TESTS
#   Stopping one service actually kills its process and clears its state. A stop
#   succeeds only when the process is truly dead. If status says "stopped" but
#   the OS still has the pid alive, the stop is a lie -- this case fails it.
#
# EXPECTED OUTCOME
#   - start exits 0 and `web` has a pid that is alive after boot.
#   - `sysg stop -s web` exits 0.
#   - `web`'s process is ACTUALLY dead per ps (pid_alive is false).
#   - status shows web state != running.
#   - the on-disk pid.xml for project `demo` no longer lists `web` (cleared).
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
STATE_DIR="$HOME/.local/share/systemg"

section "boot the single-service project"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3

STATUS="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
PID="$(unit_field "$STATUS" web pid)"
echo "web pid: $PID"
[ -n "$PID" ] && [ "$PID" != "absent" ] && [ "$PID" != "None" ]
check "$?" "web has a pid"
pid_alive "$PID"
check "$?" "web process is alive before stop"

section "stop -s web actually kills the process"
sysg stop --service web
check "$?" "stop -s web exits 0"
sleep 2

if pid_alive "$PID"; then
  check 1 "web process is actually dead after stop"
else
  check 0 "web process is actually dead after stop"
fi

section "status and pid.xml agree the service is stopped"
STATUS2="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
[ "$(unit_field "$STATUS2" web state)" != "running" ]
check "$?" "web is no longer running in status"
if grep -q "<name>web</name>" "$STATE_DIR/projects/demo/pid.xml" 2>/dev/null; then
  check 1 "web's pid.xml entry was cleared"
else
  check 0 "web's pid.xml entry was cleared"
fi

sysg stop --supervisor >/dev/null 2>&1
finish
