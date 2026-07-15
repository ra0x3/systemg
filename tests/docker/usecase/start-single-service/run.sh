#!/usr/bin/env bash
# USE CASE: start a single-service project.
#
# WHAT THIS TESTS
#   `sysg start -c stack.yaml --daemonize` on a project `demo` with one service
#   `web`. This is the bedrock: if start can't get one service up and report it
#   truthfully, nothing else can be trusted.
#
# EXPECTED OUTCOME
#   - start exits 0.
#   - `web` has a PID that is actually alive in the process tree (ps agrees).
#   - `sysg status` reports exactly one unit, `web`, state=running, under
#     project `demo`, with the SAME pid the OS shows.
#   - the on-disk pid.xml for project `demo` records that pid.
#   What `sysg status` shows and what `ps` shows MUST agree.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
STATE_DIR="$HOME/.local/share/systemg"

section "start the single-service project"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3

STATUS="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
PID="$(unit_field "$STATUS" web pid)"
echo "web pid per status: $PID"

section "status reports exactly the one service, running, under demo"
[ "$(unit_count "$STATUS")" = "1" ]
check "$?" "status shows exactly one unit"
[ "$(unit_field "$STATUS" web state)" = "running" ]
check "$?" "web is running"
[ "$(unit_field "$STATUS" web project)" = "demo" ]
check "$?" "web is grouped under project demo"

section "status and ps agree on the pid"
[ -n "$PID" ] && [ "$PID" != "absent" ] && [ "$PID" != "None" ]
check "$?" "web has a pid in status"
pid_alive "$PID"
check "$?" "that pid is actually alive per ps"

section "on-disk pid.xml records the running pid"
[ -f "$STATE_DIR/projects/demo/pid.xml" ]
check "$?" "projects/demo/pid.xml exists"
grep -q "$PID" "$STATE_DIR/projects/demo/pid.xml" 2>/dev/null
check "$?" "pid.xml records web's pid"

sysg stop --config "$CONFIG" >/dev/null 2>&1
finish
