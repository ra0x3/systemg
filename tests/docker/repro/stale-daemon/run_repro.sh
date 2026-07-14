#!/usr/bin/env bash
# Reproduces the stale-daemon bug: `sysg restart` from an upgraded (or
# downgraded) CLI is only an IPC message to the resident supervisor process,
# which never re-executes itself. The daemon keeps running the previous sysg
# build forever; only child services get re-spawned. ef4304e's recycle path
# fires solely on IPC *schema* mismatch, so version drift with a compatible
# schema leaves the old daemon in place silently.
#
# Patched behavior asserted below:
#   1. full `sysg restart --daemonize` from a drifted CLI recycles the
#      supervisor process (new pid, new binary)
#   2. services come back up under the recycled supervisor
#   3. a same-version restart does NOT recycle (pid stays)
set -u

CONFIG=/repro/sysg.config.yaml
export HOME=/root

PASS=0
FAIL=0
section() { printf '\n========== %s ==========\n' "$1"; }
check()   { if [ "$1" = "0" ]; then echo "PASS: $2"; PASS=$((PASS+1)); else echo "FAIL: $2"; FAIL=$((FAIL+1)); fi; }

supervisor_pid() { cat "$HOME/.local/share/systemg/sysg.pid" 2>/dev/null; }
supervisor_exe() { readlink "/proc/$1/exe" 2>/dev/null; }

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

section "boot supervisor with OLD binary (0.0.1)"
sysg-old start -c "$CONFIG" --daemonize
echo "start rc: $?"
sleep 3
OLD_PID="$(supervisor_pid)"
echo "old supervisor pid: $OLD_PID, exe: $(supervisor_exe "$OLD_PID")"
[ -n "$OLD_PID" ] && kill -0 "$OLD_PID" 2>/dev/null
check "$?" "old-binary supervisor is running"

section "sysg-new restart --daemonize (version drift 0.0.1 -> 0.0.2)"
sysg-new restart --config "$CONFIG" --daemonize 2>&1
RESTART_RC=$?
echo "restart rc: $RESTART_RC"
sleep 4

NEW_PID="$(supervisor_pid)"
NEW_EXE="$(supervisor_exe "$NEW_PID")"
echo "supervisor pid after restart: $NEW_PID, exe: $NEW_EXE"
[ "$RESTART_RC" -eq 0 ]; check "$?" "drifted restart exits 0"
[ -n "$NEW_PID" ] && [ "$NEW_PID" != "$OLD_PID" ]
check "$?" "supervisor process was recycled (pid changed)"
[ "$NEW_EXE" = "/usr/local/bin/sysg-new" ]
check "$?" "recycled supervisor runs the new binary"

STATUS_JSON="$(sysg-new status --config "$CONFIG" --format json 2>/dev/null)"
STEADY_STATE="$(unit_state "$STATUS_JSON" steady)"
echo "steady state: $STEADY_STATE"
[ "$STEADY_STATE" = "running" ]; check "$?" "service is running under recycled supervisor"

section "sysg-new restart again (no drift) must NOT recycle"
sysg-new restart --config "$CONFIG" --daemonize 2>&1
echo "restart rc: $?"
sleep 4
SAME_PID="$(supervisor_pid)"
echo "supervisor pid after same-version restart: $SAME_PID"
[ -n "$SAME_PID" ] && [ "$SAME_PID" = "$NEW_PID" ]
check "$?" "same-version restart keeps the supervisor process"

STATUS_JSON="$(sysg-new status --config "$CONFIG" --format json 2>/dev/null)"
STEADY_STATE="$(unit_state "$STATUS_JSON" steady)"
[ "$STEADY_STATE" = "running" ]; check "$?" "service is running after same-version restart"

section "RESULT"
echo "passed: $PASS  failed: $FAIL"
if [ "$FAIL" -eq 0 ]; then
  echo "=> GREEN: stale-daemon fixed."
  exit 0
else
  echo "=> RED: stale-daemon reproduced / not fixed."
  exit 1
fi
