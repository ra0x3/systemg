#!/usr/bin/env bash
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

launch_threads() {
  ps -T -p "$1" -o comm= | grep -c '^sysg-service-la' || true
}

sysg start -c "$CONFIG" --daemonize
check "$?" "fixed-port rolling service starts"
sleep 2
SUPERVISOR="$(cat "$HOME/.local/share/systemg/sysg.pid")"
THREADS_BEFORE="$(launch_threads "$SUPERVISOR")"
FIRST="$(tail -1 /tmp/server-pids)"

RC=0
for _ in 1 2 3; do
  sysg restart -p demo -s web >/tmp/restart.out 2>/tmp/restart.err || RC=1
done
check "$RC" "three fixed-port rolling restarts succeed"

sleep 2
STATUS="$(sysg status -p demo --format json 2>/dev/null)"
CURRENT="$(unit_field "$STATUS" web pid demo)"
[ "$CURRENT" != "$FIRST" ] && pid_alive "$CURRENT"
check "$?" "replacement generation is live on a new pid"
[ "$(http_get http://127.0.0.1:18080/health)" = "$CURRENT" ]
check "$?" "health endpoint belongs to the tracked generation"
[ "$(ps -eo args= | grep -c '[s]erver.py 18080')" = "1" ]
check "$?" "exactly one fixed-port server remains"
[ "$(cat /tmp/pre-count)" = "4" ]
check "$?" "pre_start ran once for each launch attempt"
[ "$(launch_threads "$SUPERVISOR")" = "$THREADS_BEFORE" ]
check "$?" "rolling churn leaked no generation threads"
grep -q "switching to immediate restart semantics" "$HOME/.local/share/systemg/logs/supervisor.log"
check "$?" "configured fixed port used the explicit immediate transition"

sysg stop --supervisor >/dev/null 2>&1
finish
