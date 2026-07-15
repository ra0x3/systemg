#!/usr/bin/env bash
# USE CASE: a whole-config restart with an unchanged manifest bounces everything.
#
# WHAT THIS TESTS
#   `restart -c <file>` with NO service/project selector and an UNCHANGED
#   manifest is the plain "bounce it all" case: every service is stopped and
#   restarted (new pids) and the supervisor stays up. This is the happy path the
#   defensive reconcile cases guard around -- it must keep working.
#
# EXPECTED OUTCOME
#   - Boot demo (svc1, svc2) running; record pids.
#   - `sysg restart -c <file>` exits 0.
#   - svc1 and svc2 both have NEW pids and are running with live processes.
#   - The supervisor is still up (status answers; pid file still present).
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot the config"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3
S1="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
SVC1_1="$(unit_field "$S1" svc1 pid)"
SVC2_1="$(unit_field "$S1" svc2 pid)"
echo "before -> svc1:$SVC1_1 svc2:$SVC2_1"
[ -n "$SVC1_1" ] && [ -n "$SVC2_1" ] && pid_alive "$SVC1_1" && pid_alive "$SVC2_1"
check "$?" "svc1 and svc2 alive before restart"

section "restart -c (no selector) bounces all services"
sysg restart --config "$CONFIG"
check "$?" "restart -c exits 0"
sleep 3
S2="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
SVC1_2="$(unit_field "$S2" svc1 pid)"
SVC2_2="$(unit_field "$S2" svc2 pid)"
echo "after  -> svc1:$SVC1_2 svc2:$SVC2_2"

[ -n "$SVC1_2" ] && [ "$SVC1_2" != "$SVC1_1" ] && pid_alive "$SVC1_2" && \
[ "$(unit_field "$S2" svc1 state)" = "running" ]
check "$?" "svc1 bounced (new pid, running, alive)"
[ -n "$SVC2_2" ] && [ "$SVC2_2" != "$SVC2_1" ] && pid_alive "$SVC2_2" && \
[ "$(unit_field "$S2" svc2 state)" = "running" ]
check "$?" "svc2 bounced (new pid, running, alive)"

section "supervisor is still up"
sysg status --config "$CONFIG" --format json 2>/tmp/sup.txt >/dev/null
if grep -qi "No running supervisor" /tmp/sup.txt; then
  check 1 "supervisor still answering status"
else
  check 0 "supervisor still answering status"
fi
[ -e "$HOME/.local/share/systemg/sysg.pid" ]
check "$?" "supervisor pid file still present"

sysg stop --supervisor >/dev/null 2>&1
finish
