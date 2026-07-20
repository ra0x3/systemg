#!/usr/bin/env bash
# USE CASE: start a config declaring two projects.
#
# WHAT THIS TESTS
#   `sysg start -c stack.yaml --daemonize` on a file declaring two projects,
#   `alpha` (service `api`) and `beta` (service `worker`). The first project
#   becomes primary and the rest register behind it. Both must fully boot.
#
# EXPECTED OUTCOME
#   - start exits 0.
#   - `alpha/api` and `beta/worker` are both running with distinct live pids
#     (ps agrees).
#   - `sysg status` (no -c, so it asks the resident supervisor) lists BOTH
#     units: `api` under project `alpha`, `worker` under project `beta`, each
#     with a pid the OS shows.
#   - the on-disk pid.xml for BOTH projects exists and they are distinct.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
STATE_DIR="$HOME/.local/share/systemg"

section "start the two-project config"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3

STATUS="$(sysg status --format json 2>/dev/null)"
API_PID="$(unit_field "$STATUS" api pid)"
WORKER_PID="$(unit_field "$STATUS" worker pid)"
echo "api pid per status: $API_PID"
echo "worker pid per status: $WORKER_PID"

section "resident supervisor lists both units across both projects"
[ "$(unit_count "$STATUS")" = "2" ]
check "$?" "status shows exactly two units"

section "alpha/api is running with a live pid"
[ "$(unit_field "$STATUS" api state)" = "running" ]
check "$?" "api is running"
[ "$(unit_field "$STATUS" api project)" = "alpha" ]
check "$?" "api is grouped under project alpha"
[ -n "$API_PID" ] && [ "$API_PID" != "absent" ] && [ "$API_PID" != "None" ]
check "$?" "api has a pid in status"
pid_alive "$API_PID"
check "$?" "api's pid is actually alive per ps"

section "beta/worker is running with a live pid"
[ "$(unit_field "$STATUS" worker state)" = "running" ]
check "$?" "worker is running"
[ "$(unit_field "$STATUS" worker project)" = "beta" ]
check "$?" "worker is grouped under project beta"
[ -n "$WORKER_PID" ] && [ "$WORKER_PID" != "absent" ] && [ "$WORKER_PID" != "None" ]
check "$?" "worker has a pid in status"
pid_alive "$WORKER_PID"
check "$?" "worker's pid is actually alive per ps"

section "the two services are distinct processes"
[ "$API_PID" != "$WORKER_PID" ]
check "$?" "api and worker have distinct pids"

section "on-disk pid.xml exists for both projects"
[ -f "$STATE_DIR/projects/alpha/pid.xml" ]
check "$?" "projects/alpha/pid.xml exists"
[ -f "$STATE_DIR/projects/beta/pid.xml" ]
check "$?" "projects/beta/pid.xml exists"

sysg stop --config "$CONFIG" >/dev/null 2>&1
finish
