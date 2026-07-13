#!/usr/bin/env bash
# Reproduces the lost-service bug.
#
# A service that double-forks (the way a real `serve` daemon backgrounds itself)
# exec's a parent that exits 0 immediately while a grandchild becomes the real,
# long-lived worker. sysg observes the exit-0 of the process it spawned and marks
# the unit `exited_successfully` / `done` -- a clean one-shot completion. It then
# stops supervising the unit entirely, even though restart_policy is "always" and
# the actual worker is alive and holding the port.
#
# Consequences:
#   * the real worker is an orphan sysg holds no handle to
#   * when the real worker dies, NOTHING restarts it: the unit is already "done"
#   * `sysg status` reports healthy/done for a service that is not running
#
# Asserted patched behavior:
#   1. a "serve"-intent unit is not marked done just because its spawn parent
#      exited 0 while its process group still has a live member
#   2. killing the real worker triggers a restart per restart_policy
set -u

CONFIG=/repro/sysg.config.yaml
export HOME=/root

PASS=0
FAIL=0
section() { printf '\n========== %s ==========\n' "$1"; }
check()   { if [ "$1" = "0" ]; then echo "PASS: $2"; PASS=$((PASS+1)); else echo "FAIL: $2"; FAIL=$((FAIL+1)); fi; }

unit_field() {
  printf '%s' "$1" | python3 -c '
import json,sys
name,field=sys.argv[1],sys.argv[2]
try: data=json.load(sys.stdin)
except Exception: print("noparse"); sys.exit()
for u in data.get("units",[]):
    if u.get("name")==name: print(u.get(field,"?")); break
else: print("absent")
' "$2" "$3"
}

# Match ONLY the real worker process, never this script's own command line.
# (pgrep -f on a substring also matches the container's `bash -c ...`, which
# silently makes every liveness check pass. Compare argv[0..] exactly instead.)
worker_pids() {
  ps -eo pid=,args= \
    | awk '$2 ~ /python3$/ && $3 == "/repro/daemonize.py" { print $1 }'
}
worker_count() { worker_pids | wc -l | tr -d ' '; }

section "sysg start --daemonize"
rm -f /tmp/worker.pid
sysg start --config "$CONFIG" --log-level debug --daemonize
echo "start rc: $?"
sleep 4

STATUS_JSON="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
STATE="$(unit_field "$STATUS_JSON" daemonizer state)"
LIFECYCLE="$(unit_field "$STATUS_JSON" daemonizer lifecycle)"
echo "daemonizer state=$STATE lifecycle=$LIFECYCLE"
echo "live worker PIDs: $(worker_pids | tr '\n' ' ')"

[ "$(worker_count)" -ge 1 ]; check "$?" "precondition: double-forked worker is running"

# ---- Fix #1: a live serve unit must not be reported as a completed one-shot ----
# The spawn parent exits 0, but the worker in its process group is alive, so the
# unit is RUNNING, not DONE. Reporting `done` here is what strands the service.
{ [ "$STATE" != "done" ] && [ "$LIFECYCLE" != "exited_successfully" ]; }
check "$?" "live double-forked unit is not misreported as done/exited_successfully"

section "kill the real worker out from under the daemon"
BEFORE_PIDS="$(worker_pids | tr '\n' ' ')"
echo "worker PIDs before kill: $BEFORE_PIDS"
for p in $BEFORE_PIDS; do kill -9 "$p" 2>/dev/null; done
sleep 2
echo "worker PIDs after kill: $(worker_pids | tr '\n' ' ')"
[ "$(worker_count)" -eq 0 ]; check "$?" "worker confirmed dead (no live worker PIDs)"

section "wait out several monitor ticks for an auto-restart"
sleep 14

RECOVER_JSON="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
STATE_AFTER="$(unit_field "$RECOVER_JSON" daemonizer state)"
AFTER_PIDS="$(worker_pids | tr '\n' ' ')"
echo "daemonizer state after kill: $STATE_AFTER"
echo "worker PIDs after wait: ${AFTER_PIDS:-<none>}"

# ---- Fix #2: the daemon noticed the dead worker and restarted it ----
[ "$(worker_count)" -ge 1 ]; check "$?" "daemonizer was auto-restarted after its real process died"

section "control: an ordinary unit still behaves"
ANCHOR_STATE="$(unit_field "$RECOVER_JSON" anchor state)"
echo "anchor state: $ANCHOR_STATE"
[ "$ANCHOR_STATE" = "running" ]; check "$?" "anchor unaffected"

sysg stop --config "$CONFIG" >/dev/null 2>&1

section "RESULT"
echo "passed: $PASS  failed: $FAIL"
if [ "$FAIL" -eq 0 ]; then
  echo "=> GREEN: lost-service fixed."
  exit 0
else
  echo "=> RED: lost-service reproduced / not fixed."
  exit 1
fi
