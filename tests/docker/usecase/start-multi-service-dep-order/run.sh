#!/usr/bin/env bash
# USE CASE: start a multi-service project with a depends_on chain.
#
# WHAT THIS TESTS
#   `sysg start -c stack.yaml --daemonize` on a 3-service project with a
#   depends_on chain (a <- b <- c). Prod bug: a single project with N services
#   only surfaced ONE in status.
#
# EXPECTED OUTCOME
#   - start exits 0.
#   - ALL THREE services (a, b, c) are running with distinct live pids that the
#     OS agrees are alive (ps agrees).
#   - `sysg status` reports exactly 3 units, all under project `demo`.
#   - each pid is alive; the three pids are pairwise distinct.
#   - dependency order was honored (a before b before c). We can't easily assert
#     wall-clock order, so we assert all three are present and running.
#   - the on-disk pid.xml for project `demo` records all three pids.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
STATE_DIR="$HOME/.local/share/systemg"

section "start the multi-service project"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3

STATUS="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
PID_A="$(unit_field "$STATUS" a pid)"
PID_B="$(unit_field "$STATUS" b pid)"
PID_C="$(unit_field "$STATUS" c pid)"
echo "pids per status: a=$PID_A b=$PID_B c=$PID_C"

section "status reports exactly the three services, running, under demo"
[ "$(unit_count "$STATUS")" = "3" ]
check "$?" "status shows exactly three units"
for svc in a b c; do
  [ "$(unit_field "$STATUS" "$svc" state)" = "running" ]
  check "$?" "$svc is running"
  [ "$(unit_field "$STATUS" "$svc" project)" = "demo" ]
  check "$?" "$svc is grouped under project demo"
done

section "each service has a live pid"
for svc in a b c; do
  PID="$(unit_field "$STATUS" "$svc" pid)"
  [ -n "$PID" ] && [ "$PID" != "absent" ] && [ "$PID" != "None" ]
  check "$?" "$svc has a pid in status"
  pid_alive "$PID"
  check "$?" "$svc's pid is actually alive per ps"
done

section "the three pids are pairwise distinct"
[ "$PID_A" != "$PID_B" ] && [ "$PID_B" != "$PID_C" ] && [ "$PID_A" != "$PID_C" ]
check "$?" "a, b, c have distinct pids"

section "on-disk pid.xml records all three running pids"
[ -f "$STATE_DIR/projects/demo/pid.xml" ]
check "$?" "projects/demo/pid.xml exists"
for svc in a b c; do
  PID="$(unit_field "$STATUS" "$svc" pid)"
  grep -q "$PID" "$STATE_DIR/projects/demo/pid.xml" 2>/dev/null
  check "$?" "pid.xml records $svc's pid"
done

sysg stop --config "$CONFIG" >/dev/null 2>&1
finish
