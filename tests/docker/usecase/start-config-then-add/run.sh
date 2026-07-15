#!/usr/bin/env bash
# USE CASE: adding a project from a second file must not disturb the first.
#
# WHAT THIS TESTS
#   Boot project `first` from first.yaml, confirm it is up, then
#   `sysg start -c second.yaml` to register project `second`. Adding the second
#   project must leave the first project's process completely untouched -- same
#   pid, still alive. This is the sibling-isolation guarantee on the ADD path
#   (the cross-project teardown bug's cousin).
#
# EXPECTED OUTCOME
#   - first_svc is running after the first start; record its pid.
#   - After `start -c second.yaml`, second_svc comes up running AND first_svc's
#     pid is UNCHANGED and still alive.
#   - status shows both projects.
set -u
. /usecase/lib.sh

section "boot the first project"
sysg start --config /usecase/first.yaml --daemonize
check "$?" "start -c first.yaml exits 0"
sleep 3
S1="$(sysg status --format json 2>/dev/null)"
FIRST_PID="$(unit_field "$S1" first_svc pid first)"
echo "first_svc pid: $FIRST_PID"
[ "$(unit_field "$S1" first_svc state first)" = "running" ]
check "$?" "first_svc is running"
pid_alive "$FIRST_PID"
check "$?" "first_svc pid is alive"

section "add the second project -- first must be untouched"
sysg start --config /usecase/second.yaml --daemonize
check "$?" "start -c second.yaml exits 0"
sleep 3
S2="$(sysg status --format json 2>/dev/null)"

[ "$(unit_field "$S2" second_svc state second)" = "running" ]
check "$?" "second_svc is running after add"

FIRST_PID_2="$(unit_field "$S2" first_svc pid first)"
echo "first_svc pid after add: $FIRST_PID_2"
[ "$FIRST_PID_2" = "$FIRST_PID" ]
check "$?" "first_svc pid UNCHANGED by adding the second project"
pid_alive "$FIRST_PID"
check "$?" "first_svc process still alive after add"

[ "$(unit_count "$S2")" = "2" ]
check "$?" "status shows both projects"

sysg stop --supervisor >/dev/null 2>&1
finish
