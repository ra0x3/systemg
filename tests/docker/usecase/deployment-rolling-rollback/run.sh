#!/usr/bin/env bash
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

launch_threads() {
  ps -T -p "$1" -o comm= | grep -c '^sysg-service-la' || true
}

echo good > /tmp/mode
sysg start -c "$CONFIG" --daemonize
check "$?" "rolling service starts"
sleep 2
SUPERVISOR="$(cat "$HOME/.local/share/systemg/sysg.pid")"
THREADS_BEFORE="$(launch_threads "$SUPERVISOR")"
OLD="$(tail -1 /tmp/server-pids)"

echo fail > /tmp/mode
sysg restart -p demo -s web >/tmp/fail.out 2>/tmp/fail.err
RC=$?
cat /tmp/fail.err
[ "$RC" != "0" ]
check "$?" "non-port candidate failure rejects the restart"
stderr_has_code SG0102 /tmp/fail.err
check "$?" "candidate failure preserves its typed cause"
pid_alive "$OLD"
check "$?" "failed candidate leaves the old generation serving"
STATUS="$(sysg status -p demo --format json 2>/dev/null)"
[ "$(unit_field "$STATUS" web pid demo)" = "$OLD" ] && [ "$(unit_field "$STATUS" web state demo)" = "running" ]
check "$?" "rollback restores runtime ownership and lifecycle state"
[ "$(cat /tmp/pre-count)" = "2" ]
check "$?" "failed candidate ran pre_start exactly once"
sleep 2
[ "$(launch_threads "$SUPERVISOR")" = "$THREADS_BEFORE" ]
check "$?" "rollback leaked no generation threads"

echo good > /tmp/mode
sysg restart -p demo -s web >/tmp/recover.out 2>/tmp/recover.err
check "$?" "service can restart successfully after rollback"
NEW="$(tail -1 /tmp/server-pids)"
[ "$NEW" != "$OLD" ] && pid_alive "$NEW" && ! pid_alive "$OLD"
check "$?" "later success retires the restored old generation"

sysg stop --supervisor >/dev/null 2>&1
finish
