#!/usr/bin/env bash
# USE CASE: status never presents unsupervised disk state as a HEALTHY stack.
#
# WHAT THIS TESTS
#   The core status lie this rebuild kills: after the supervisor is gone, status
#   used to read the stale state.xml and render "HEALTHY / exit 0" as if a
#   supervisor were managing everything. Now, with NO supervisor:
#     - status exits 2 (offline is a failing condition, never a clean 0),
#     - stderr carries a typed SG0206 banner naming the orphaned processes,
#     - the overview reads OFFLINE, not HEALTHY,
#     - the disk-read units are STILL shown (so you see what survived), and each
#       shown-running unit is actually alive in the process table.
#
# EXPECTED OUTCOME
#   - Boot demo (web, api); record their pids.
#   - Kill the supervisor process only (services survive) and clear its runtime.
#   - `sysg status` -> exit 2, SG0206 on stderr, OFFLINE overview, web+api rows
#     present and their pids still alive (unsupervised survivors).
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
STATE_DIR="$HOME/.local/share/systemg"

section "boot the config"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3
S1="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
WEB1="$(unit_field "$S1" web pid)"
API1="$(unit_field "$S1" api pid)"
echo "before -> web:$WEB1 api:$API1"
[ -n "$WEB1" ] && [ -n "$API1" ] && pid_alive "$WEB1" && pid_alive "$API1"
check "$?" "web and api running under the supervisor"

section "kill the supervisor only; services survive as orphans"
SUP="$(cat "$STATE_DIR/sysg.pid")"
kill -9 "$SUP"
rm -f "$STATE_DIR/sysg.pid" "$STATE_DIR/control.sock"
sleep 1
pid_alive "$WEB1" && pid_alive "$API1"
check "$?" "web and api STILL alive after the supervisor died"

section "status reports OFFLINE, not a supervised HEALTHY"
sysg status --config "$CONFIG" >/tmp/out.txt 2>/tmp/err.txt
RC=$?
echo "status rc=$RC"
echo "--- stderr ---"; cat /tmp/err.txt
[ "$RC" = "2" ]
check "$?" "status exits 2 (offline, never a clean 0)"
stderr_has_code SG0206 /tmp/err.txt
check "$?" "stderr names SG0206 (supervisor offline)"
grep -q "Status:.*OFFLINE" /tmp/out.txt
check "$?" "overview title reads OFFLINE"
grep -q "Status:.*HEALTHY" /tmp/out.txt && TITLE_HEALTHY=1 || TITLE_HEALTHY=0
[ "$TITLE_HEALTHY" = "0" ]
check "$?" "overview title does NOT claim HEALTHY while unsupervised"

section "the disk-read survivors are still shown and still alive"
SJSON="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
[ "$(unit_field "$SJSON" web pid)" = "$WEB1" ]
check "$?" "web still shown on its original pid"
[ "$(unit_field "$SJSON" api pid)" = "$API1" ]
check "$?" "api still shown on its original pid"
pid_alive "$WEB1" && pid_alive "$API1"
check "$?" "both survivors still alive per ps"

pkill -9 sleep 2>/dev/null || true
finish
