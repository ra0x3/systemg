#!/usr/bin/env bash
# USE CASE: stopping a project leaves sibling projects untouched.
#
# WHAT THIS TESTS
#   Stopping a project stops ALL its services and leaves OTHER projects
#   completely untouched (the sibling-isolation guarantee). `stop -p alpha`
#   must take down a1 and a2 while beta's b1 keeps running on the exact same
#   pid it had before -- no collateral damage across project boundaries.
#
# EXPECTED OUTCOME
#   - start exits 0; a1, a2 (project alpha) and b1 (project beta) all running.
#   - `sysg stop -p alpha` exits 0.
#   - a1 and a2 are no longer running (alpha stopped).
#   - b1 is still running on its ORIGINAL pid, still alive per ps (beta untouched).
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot two projects (alpha: a1,a2  beta: b1)"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3

STATUS="$(sysg status --format json 2>/dev/null)"
B1_PID="$(unit_field "$STATUS" b1 pid beta)"
echo "b1 pid: $B1_PID"

[ "$(unit_field "$STATUS" a1 state alpha)" = "running" ]
check "$?" "a1 is running before stop"
[ "$(unit_field "$STATUS" a2 state alpha)" = "running" ]
check "$?" "a2 is running before stop"
[ "$(unit_field "$STATUS" b1 state beta)" = "running" ]
check "$?" "b1 is running before stop"

section "stop -p alpha takes down alpha only"
sysg stop --project alpha
check "$?" "stop -p alpha exits 0"
sleep 2

STATUS2="$(sysg status --format json 2>/dev/null)"

[ "$(unit_field "$STATUS2" a1 state alpha)" != "running" ]
check "$?" "a1 is no longer running (alpha stopped)"
[ "$(unit_field "$STATUS2" a2 state alpha)" != "running" ]
check "$?" "a2 is no longer running (alpha stopped)"

section "beta is completely untouched"
[ "$(unit_field "$STATUS2" b1 state beta)" = "running" ]
check "$?" "b1 is still running (beta untouched)"
[ "$(unit_field "$STATUS2" b1 pid beta)" = "$B1_PID" ]
check "$?" "b1's pid is unchanged"
pid_alive "$B1_PID"
check "$?" "b1's process is still alive per ps"

sysg stop --supervisor >/dev/null 2>&1
finish
