#!/usr/bin/env bash
# USE CASE: a hung pre_start is bounded and CANNOT freeze the supervisor.
#
# WHAT THIS TESTS (CRITICAL real dogfooding bug)
#   pre_start commands run inside the supervisor's single-writer owner thread.
#   An UNBOUNDED pre_start that hangs (e.g. a network/proxy call that never
#   returns) held the owner lock for HOURS, so every later stop/restart/start —
#   across ALL projects — queued behind it forever, and the wedged op's stale
#   progress line ("running pre-start for X") bled into unrelated commands.
#   A pre_start must be time-bounded: on timeout it is killed, its start fails,
#   the owner thread is released, and the supervisor keeps serving.
#
# EXPECTED OUTCOME
#   - `hung` (pre_start: sleep 3000) is killed at the pre-start timeout; it does
#     not run forever.
#   - AFTER the hung pre-start, the supervisor is RESPONSIVE: a status query and
#     a stop of the sibling service both return QUICKLY (not blocked for hours).
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
# Bound pre-starts to 3s for this test so the hung one is killed fast.
export SYSG_PRE_START_TIMEOUT_SECS=3

section "start a project with a hung pre_start service"
# `hung` has pre_start: sleep 3000 (never returns). With the bound it is killed
# at 3s and its start fails; the sibling comes up.
sysg start --config "$CONFIG" --daemonize 2>/tmp/start.err
echo "start rc: $?"
sleep 6

section "the supervisor is NOT frozen — commands still respond quickly"
# A status query must return promptly (the owner thread was released, not held
# for hours by the hung pre-start).
T0="$(date +%s)"
sysg status --config "$CONFIG" >/dev/null 2>&1
T1="$(date +%s)"
echo "status responded in $((T1 - T0))s"
[ "$((T1 - T0))" -lt 10 ]
check "$?" "status responds quickly (supervisor not wedged by the hung pre-start)"

# A mutation (stop the sibling) must also go through promptly.
T0="$(date +%s)"
sysg stop --config "$CONFIG" -s sibling >/dev/null 2>&1
RC=$?
T2="$(date +%s)"
echo "stop -s sibling rc=$RC in $((T2 - T0))s"
[ "$RC" = "0" ] && [ "$((T2 - T0))" -lt 10 ]
check "$?" "a mutation runs promptly (owner thread was released, not held by pre-start)"

section "the hung pre-start's service did not stay running forever"
S="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
HUNG_STATE="$(unit_field "$S" hung state demo)"
echo "hung state: $HUNG_STATE"
[ "$HUNG_STATE" != "running" ]
check "$?" "the hung pre-start service is not left running"

sysg stop --supervisor >/dev/null 2>&1
finish
