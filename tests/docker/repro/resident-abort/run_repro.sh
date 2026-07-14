#!/usr/bin/env bash
# Reproduces the resident-abort bug: with a supervisor already running, loading
# another project (`sysg start -c b.yaml`) or restarting one (`sysg restart -p`)
# goes through Supervisor::start_project_services, which aborts the whole
# project on the first failing unit. Everything ordered after the bad unit is
# never started, and the CLI gets an SG0001 error even though the healthy units
# could have come up.
#
# This is the resident-supervisor sibling of the boot-abort repro: ca79691
# fixed cold boot only; the IPC add/restart paths still abort.
#
# Patched behavior asserted below:
#   1. loading project B starts its healthy units despite one bad unit
#   2. `sysg restart -p` on that project succeeds and keeps healthy units up
#   3. the project stays managed by the supervisor throughout
set -u

CONFIG_A=/repro/a.config.yaml
CONFIG_B=/repro/b.config.yaml
export HOME=/root

PASS=0
FAIL=0
section() { printf '\n========== %s ==========\n' "$1"; }
check()   { if [ "$1" = "0" ]; then echo "PASS: $2"; PASS=$((PASS+1)); else echo "FAIL: $2"; FAIL=$((FAIL+1)); fi; }

# unit_state <json> <unit-name> -> prints the unit's state, or "absent"
unit_state() {
  printf '%s' "$1" | python3 -c '
import json,sys
name=sys.argv[1]
try: data=json.load(sys.stdin)
except Exception: print("noparse"); sys.exit()
for u in data.get("units",[]):
    if u.get("name")==name: print(u.get("state","?")); break
else: print("absent")
' "$2"
}

section "boot resident supervisor with project A"
sysg start --config "$CONFIG_A" --log-level debug --daemonize
echo "start A rc: $?"
sleep 3

STATUS_JSON="$(sysg status --config "$CONFIG_A" --format json 2>/dev/null)"
ANCHOR_STATE="$(unit_state "$STATUS_JSON" anchor)"
echo "anchor state: $ANCHOR_STATE"
[ "$ANCHOR_STATE" = "running" ]; check "$?" "supervisor is resident; anchor is running"

# ---- Fix #1: loading a project with one bad unit must not abort the rest ----
section "sysg start -c b.config.yaml against the resident supervisor"
START_B_OUT="$(sysg start --config "$CONFIG_B" --log-level debug --daemonize 2>&1)"
START_B_RC=$?
echo "$START_B_OUT"
echo "start B rc: $START_B_RC"
sleep 3

STATUS_JSON="$(sysg status --config "$CONFIG_B" --format json 2>/dev/null)"
GOOD_STATE="$(unit_state "$STATUS_JSON" good_one)"
TAIL_STATE="$(unit_state "$STATUS_JSON" zz_tail)"
echo "good_one state: $GOOD_STATE"
echo "zz_tail state:  $TAIL_STATE"
[ "$GOOD_STATE" = "running" ]; check "$?" "good_one is running despite aaa_boom failing"
[ "$TAIL_STATE" = "running" ]; check "$?" "zz_tail is running despite aaa_boom failing"

# ---- Fix #2: restart -p must not abort on the bad unit either ----
section "sysg restart -p resident-b"
RESTART_OUT="$(sysg restart -p resident-b 2>&1)"
RESTART_RC=$?
echo "$RESTART_OUT"
echo "restart rc: $RESTART_RC"
sleep 3

[ "$RESTART_RC" -eq 0 ]; check "$?" "restart -p resident-b exits 0"
printf '%s' "$RESTART_OUT" | grep -q "not managed by this supervisor"
[ "$?" -ne 0 ]; check "$?" "project resident-b is still managed"

STATUS_JSON="$(sysg status --config "$CONFIG_B" --format json 2>/dev/null)"
GOOD_STATE="$(unit_state "$STATUS_JSON" good_one)"
TAIL_STATE="$(unit_state "$STATUS_JSON" zz_tail)"
ANCHOR_JSON="$(sysg status --config "$CONFIG_A" --format json 2>/dev/null)"
ANCHOR_STATE="$(unit_state "$ANCHOR_JSON" anchor)"
echo "good_one state after restart: $GOOD_STATE"
echo "zz_tail state after restart:  $TAIL_STATE"
echo "anchor state after restart:   $ANCHOR_STATE"
[ "$GOOD_STATE" = "running" ]; check "$?" "good_one is running after restart"
[ "$TAIL_STATE" = "running" ]; check "$?" "zz_tail is running after restart"
[ "$ANCHOR_STATE" = "running" ]; check "$?" "restart -p resident-b did not kill project A's anchor"

section "supervisor log tail"
tail -n 40 "$HOME/.local/share/systemg/logs/supervisor.log" 2>/dev/null || true

section "RESULT"
echo "passed: $PASS  failed: $FAIL"
if [ "$FAIL" -eq 0 ]; then
  echo "=> GREEN: resident-abort fixed."
  exit 0
else
  echo "=> RED: resident-abort reproduced / not fixed."
  exit 1
fi
