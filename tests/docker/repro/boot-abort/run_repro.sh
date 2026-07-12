#!/usr/bin/env bash
# Reproduces the boot-abort bug: one service failing to start (here, a port
# conflict) makes the whole supervisor abort during boot and exit before
# publishing its control socket. The daemon dies silently (stderr -> /dev/null),
# so `sysg start --daemonize` returns 0 and `sysg status` reports "No running
# supervisor" with no explanation.
#
# Patched behavior asserted below:
#   1. a bad unit does NOT abort the supervisor    (Fix #1)
#   2. `start --daemonize` reports boot failure instead of a silent exit 0 (Fix #2)
set -u

CONFIG=/repro/sysg.config.yaml
BROKEN=/repro/broken.config.yaml
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

section "pre-hold port 8099 so port_hog cannot bind"
python3 -c 'import socket,time; s=socket.socket(); s.bind(("127.0.0.1",8099)); s.listen(); time.sleep(3000)' &
HOG_PID=$!
sleep 1
echo "external hog PID: $HOG_PID"

section "sysg start --daemonize (one service will fail to bind)"
sysg start --config "$CONFIG" --log-level debug --daemonize
START_RC=$?
echo "start rc: $START_RC"
sleep 3

# ---- Fix #1: the supervisor came up despite the failing unit ----
section "sysg status --format json"
STATUS_JSON="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
echo "$STATUS_JSON" | python3 -m json.tool 2>/dev/null || echo "$STATUS_JSON"

GOOD_STATE="$(unit_state "$STATUS_JSON" good_service)"
echo "good_service state: $GOOD_STATE"
[ "$GOOD_STATE" = "running" ]; check "$?" "supervisor came up; good_service is running"

# ---- Fix #1: bad unit recovers once the port frees ----
section "free port 8099; port_hog should recover via restart policy"
kill "$HOG_PID" 2>/dev/null
sleep 8
RECOVER_JSON="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
HOG_STATE="$(unit_state "$RECOVER_JSON" port_hog)"
echo "port_hog state after port freed: $HOG_STATE"
LIVE_HOGS="$(pgrep -fc '8099' || true)"
{ [ "$HOG_STATE" = "running" ] || [ "${LIVE_HOGS:-0}" -ge 1 ]; }; check "$?" "port_hog recovered after port freed"

section "stop supervisor before negative case"
sysg stop --config "$CONFIG" >/dev/null 2>&1
sleep 2

# ---- Fix #2: a genuinely broken config exits non-zero, not silent 0 ----
section "sysg start --daemonize with a broken config (unknown dependency)"
sysg start --config "$BROKEN" --log-level debug --daemonize
BROKEN_RC=$?
echo "broken start rc: $BROKEN_RC"
[ "$BROKEN_RC" -ne 0 ]; check "$?" "broken config makes start exit non-zero"

section "RESULT"
echo "passed: $PASS  failed: $FAIL"
if [ "$FAIL" -eq 0 ]; then
  echo "=> GREEN: boot-abort fixed."
  exit 0
else
  echo "=> RED: boot-abort reproduced / not fixed."
  exit 1
fi
