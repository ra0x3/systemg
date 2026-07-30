#!/usr/bin/env bash
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

launch_threads() {
  ps -T -p "$1" -o comm= | grep -c '^sysg-service-la' || true
}

launch_threads_settle() {
  for _ in $(seq 1 5); do
    [ "$(launch_threads "$1")" = "$2" ] && return 0
    sleep 1
  done
  return 1
}

echo 18082 > /tmp/active-slot
sysg start -c "$CONFIG" --daemonize
check "$?" "blue/green service starts on slot zero"
sleep 2
SUPERVISOR="$(cat "$HOME/.local/share/systemg/sysg.pid")"
THREADS_BEFORE="$(launch_threads "$SUPERVISOR")"
OLD="$(http_get http://127.0.0.1:18082/health)"

sysg restart -p demo -s web >/tmp/first.out 2>/tmp/first.err
check "$?" "healthy candidate switches traffic"
ACTIVE="$(cat /tmp/active-slot)"
FIRST_NEW="$(http_get http://127.0.0.1:$ACTIVE/health)"
[ "$ACTIVE" = "18083" ] && [ "$FIRST_NEW" != "$OLD" ] && ! pid_alive "$OLD"
check "$?" "successful switch retires only the old generation"
[ "$(cat /tmp/pre-count)" = "2" ]
check "$?" "blue/green pre_start runs once per generation"

echo 18082 > /tmp/fail-slot
sysg restart -p demo -s web >/tmp/candidate.out 2>/tmp/candidate.err
RC=$?
[ "$RC" != "0" ] && stderr_has_code SG0102 /tmp/candidate.err
check "$?" "failed candidate rejects the deployment with its typed cause"
[ "$(cat /tmp/active-slot)" = "18083" ] && pid_alive "$FIRST_NEW"
check "$?" "failed candidate preserves active traffic and old generation"
rm /tmp/fail-slot

touch /tmp/switch-fail-once
sysg restart -p demo -s web >/tmp/switch.out 2>/tmp/switch.err
RC=$?
[ "$RC" != "0" ]
check "$?" "failed traffic switch rejects the deployment"
[ "$(cat /tmp/active-slot)" = "18083" ] && pid_alive "$FIRST_NEW"
check "$?" "failed traffic switch rolls traffic back"

touch /tmp/verify-fail
sysg restart -p demo -s web >/tmp/verify.out 2>/tmp/verify.err
RC=$?
[ "$RC" != "0" ]
check "$?" "failed post-switch verification rejects the deployment"
[ "$(cat /tmp/active-slot)" = "18083" ] && pid_alive "$FIRST_NEW"
check "$?" "failed post-switch verification restores active traffic"
rm /tmp/verify-fail

sysg restart -p demo -s web >/tmp/final.out 2>/tmp/final.err
check "$?" "deployment succeeds after repeated rollback paths"
FINAL_SLOT="$(cat /tmp/active-slot)"
FINAL_PID="$(http_get http://127.0.0.1:$FINAL_SLOT/health)"
[ "$FINAL_SLOT" = "18082" ] && [ "$FINAL_PID" != "$FIRST_NEW" ] && ! pid_alive "$FIRST_NEW"
check "$?" "final commit owns traffic and retires restored old generation"
[ "$(cat /tmp/pre-count)" = "6" ]
check "$?" "each blue/green launch attempt ran pre_start once"
launch_threads_settle "$SUPERVISOR" "$THREADS_BEFORE"
check "$?" "blue/green failures leaked no generation threads"

sysg stop --supervisor >/dev/null 2>&1
finish
