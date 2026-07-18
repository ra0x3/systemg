#!/usr/bin/env bash
# USE CASE: a `sysg restart` across a version upgrade TRANSFERS MANAGEMENT of the
# services from the old supervisor to the new one — not merely restarts them.
#
# THE PROD PAIN
#   You run sysg v1.0, it manages your stack. You upgrade the binary to v2.0.
#   You `sysg restart`. The services must stop being owned by the v1.0 daemon and
#   start being owned by v2.0: the old daemon gone, the new daemon actively
#   supervising the new processes (so a crash is respawned by v2.0). If the
#   handoff is fake — old daemon lingers, or the new pids are unmanaged orphans —
#   the next crash is never recovered and status lies.
#
# HARNESS
#   `sysg-old` (v0.0.1 resident daemon) + `sysg` (real CLI that drives restart).
#
# HARD INVARIANTS
#   - the OLD supervisor pid is DEAD after the handoff (not lingering),
#   - the live supervisor answers at the CLI's version (drift resolved),
#   - services run on NEW pids,
#   - MANAGEMENT transferred: kill a service and v2.0 respawns it on a new pid,
#   - no orphan from the old generation (every sleep has a live parent).
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
CLI_VERSION="$(sysg --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1)"
STATE_DIR="$HOME/.local/share/systemg"

section "boot the stack with the OLD (v0.0.1) supervisor"
sysg-old start --config "$CONFIG" --daemonize
check "$?" "old supervisor start exits 0"
sleep 3
OLD_SUP="$(cat "$STATE_DIR/sysg.pid" 2>/dev/null | tr -d ' ')"
echo "old supervisor pid: $OLD_SUP (cli v$CLI_VERSION)"
[ -n "$OLD_SUP" ] && pid_alive "$OLD_SUP"
check "$?" "old supervisor pid recorded and alive"
S1="$(sysg-old status --config "$CONFIG" --format json 2>/dev/null)"
WEB1="$(unit_field "$S1" web pid demo)"
API1="$(unit_field "$S1" api pid demo)"
echo "before -> web:$WEB1 api:$API1"
[ -n "$WEB1" ] && [ -n "$API1" ] && pid_alive "$WEB1" && pid_alive "$API1"
check "$?" "web and api running under the old supervisor"

section "restart with the NEW CLI recycles + hands off management"
sysg restart --config "$CONFIG" 2>/tmp/r.txt
RC=$?
cat /tmp/r.txt
[ "$RC" = "0" ]
check "$?" "restart (recycle) exits 0"
sleep 3

section "the OLD supervisor is gone"
if [ -n "$OLD_SUP" ] && pid_alive "$OLD_SUP"; then
  check 1 "old supervisor pid $OLD_SUP is dead (no lingering v0.0.1 daemon)"
else
  check 0 "old supervisor pid $OLD_SUP is dead (no lingering v0.0.1 daemon)"
fi
NEW_SUP="$(cat "$STATE_DIR/sysg.pid" 2>/dev/null | tr -d ' ')"
echo "new supervisor pid: $NEW_SUP"
[ -n "$NEW_SUP" ] && [ "$NEW_SUP" != "$OLD_SUP" ] && pid_alive "$NEW_SUP"
check "$?" "a NEW supervisor pid is live (not the old one)"

section "the live supervisor answers at the CLI version (drift resolved)"
sysg status --config "$CONFIG" >/tmp/sup.txt 2>&1
grep -qi "No running supervisor" /tmp/sup.txt && ANS=1 || ANS=0
[ "$ANS" = "0" ]
check "$?" "new supervisor answers the CLI (no version-drift rejection)"

section "services are back on NEW pids"
S2="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
WEB2="$(unit_field "$S2" web pid demo)"
API2="$(unit_field "$S2" api pid demo)"
echo "after  -> web:$WEB2 api:$API2"
[ -n "$WEB2" ] && pid_alive "$WEB2" && [ "$WEB2" != "$WEB1" ]
check "$?" "web running on a NEW pid under the new supervisor"
[ -n "$API2" ] && pid_alive "$API2" && [ "$API2" != "$API1" ]
check "$?" "api running on a NEW pid under the new supervisor"

section "MANAGEMENT transferred: v2.0 respawns a killed service"
kill -9 "$WEB2" 2>/dev/null
echo "killed web pid $WEB2; waiting for the new supervisor to respawn it"
RESPAWNED=0
i=0
while [ "$i" -lt 20 ]; do
  sleep 1
  S3="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
  WEB3="$(unit_field "$S3" web pid demo)"
  if [ -n "$WEB3" ] && [ "$WEB3" != "absent" ] && [ "$WEB3" != "$WEB2" ] && pid_alive "$WEB3"; then
    RESPAWNED=1
    echo "web respawned on new pid $WEB3 (was $WEB2)"
    break
  fi
  i=$((i+1))
done
[ "$RESPAWNED" = "1" ]
check "$?" "new supervisor ACTIVELY manages web (respawned it after a kill)"

section "no orphan from the old generation"
ORPHANS=0
for sp in $(pgrep -x sleep); do
  PP="$(ps -o ppid= -p "$sp" | tr -d ' ')"
  pid_alive "$PP" || ORPHANS=$((ORPHANS+1))
done
echo "orphaned sleeps (dead parent): $ORPHANS (expected 0)"
[ "$ORPHANS" = "0" ]
check "$?" "no orphaned service process survived the handoff"

sysg stop --supervisor >/dev/null 2>&1
finish
