#!/usr/bin/env bash
# USE CASE: two projects declare a SAME-NAMED service ("worker") with an
# identical command. Under the old single shared pid.xml/state.xml this
# collided — one project's worker overwrote the other's record, and every
# project's status surfaced the other's units. With per-project state
# directories ({state_dir}/projects/{id}/*) the two are structurally isolated.
#
# Proven here end-to-end against the real binary:
#   - both workers run with DISTINCT pids (no pid-file collision)
#   - `status -p alpha` shows exactly alpha's worker; likewise beta
#   - neither project's status leaks the other's unit or shows [orphaned]
#   - on disk, projects/alpha/ and projects/beta/ each hold their own pid.xml
#   - restarting alpha's worker leaves beta's worker pid untouched
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
STATE_DIR="$HOME/.local/share/systemg"

# pid of the "worker" unit belonging to <project> in a status json blob.
worker_pid_for_project() {
  printf '%s' "$1" | python3 -c '
import json,sys
proj=sys.argv[1]
try: data=json.load(sys.stdin)
except Exception: print("noparse"); sys.exit()
for u in data.get("units",[]):
    if u.get("name")=="worker":
        p=u.get("project") or {}
        if p.get("id")==proj:
            proc=u.get("process") or {}
            print(proc.get("pid")); break
else: print("absent")
' "$2"
}

# count of units in a status json blob.
unit_count() {
  printf '%s' "$1" | python3 -c '
import json,sys
try: data=json.load(sys.stdin)
except Exception: print(-1); sys.exit()
print(len(data.get("units",[])))
'
}

# count of units whose name is "worker" belonging to <project>.
worker_units_for_project() {
  printf '%s' "$1" | python3 -c '
import json,sys
proj=sys.argv[1]
try: data=json.load(sys.stdin)
except Exception: print(-1); sys.exit()
n=0
for u in data.get("units",[]):
    p=u.get("project") or {}
    if u.get("name")=="worker" and p.get("id")==proj: n+=1
print(n)
' "$2"
}

# count of [orphaned] units (state == "orphaned") in a status blob.
orphan_count() {
  printf '%s' "$1" | python3 -c '
import json,sys
try: data=json.load(sys.stdin)
except Exception: print(-1); sys.exit()
print(sum(1 for u in data.get("units",[]) if u.get("state")=="orphaned"))
'
}

section "start both projects from one file"
sysg start --config "$CONFIG" --daemonize
echo "start rc: $?"
sleep 3

FULL="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
ALPHA_PID="$(worker_pid_for_project "$FULL" alpha)"
BETA_PID="$(worker_pid_for_project "$FULL" beta)"
echo "alpha worker pid: $ALPHA_PID   beta worker pid: $BETA_PID"

section "both same-named workers run with distinct pids"
[ -n "$ALPHA_PID" ] && [ "$ALPHA_PID" != "absent" ] && [ "$ALPHA_PID" != "None" ]
check "$?" "alpha/worker has a pid"
[ -n "$BETA_PID" ] && [ "$BETA_PID" != "absent" ] && [ "$BETA_PID" != "None" ]
check "$?" "beta/worker has a pid"
[ "$ALPHA_PID" != "$BETA_PID" ]
check "$?" "alpha and beta workers have DISTINCT pids (no collision)"

section "both pids are actually alive per ps"
kill -0 "$ALPHA_PID" 2>/dev/null; check "$?" "alpha worker pid is alive"
kill -0 "$BETA_PID" 2>/dev/null; check "$?" "beta worker pid is alive"

section "per-project status shows exactly that project's own worker"
ALPHA_STATUS="$(sysg status --project alpha --format json 2>/dev/null)"
BETA_STATUS="$(sysg status --project beta --format json 2>/dev/null)"

[ "$(worker_units_for_project "$ALPHA_STATUS" alpha)" = "1" ]
check "$?" "status -p alpha lists alpha's worker"
[ "$(worker_units_for_project "$ALPHA_STATUS" beta)" = "0" ]
check "$?" "status -p alpha does NOT leak beta's worker"
[ "$(unit_count "$ALPHA_STATUS")" = "1" ]
check "$?" "status -p alpha shows exactly one unit"

[ "$(worker_units_for_project "$BETA_STATUS" beta)" = "1" ]
check "$?" "status -p beta lists beta's worker"
[ "$(worker_units_for_project "$BETA_STATUS" alpha)" = "0" ]
check "$?" "status -p beta does NOT leak alpha's worker"
[ "$(unit_count "$BETA_STATUS")" = "1" ]
check "$?" "status -p beta shows exactly one unit"

section "no orphaned units anywhere (shared-state leak signature)"
[ "$(orphan_count "$FULL")" = "0" ]
check "$?" "aggregate status has zero [orphaned] units"

section "each project owns its own on-disk state directory"
[ -f "$STATE_DIR/projects/alpha/pid.xml" ]
check "$?" "projects/alpha/pid.xml exists"
[ -f "$STATE_DIR/projects/beta/pid.xml" ]
check "$?" "projects/beta/pid.xml exists"
# alpha's pid file names alpha's pid and NOT beta's.
grep -q "$ALPHA_PID" "$STATE_DIR/projects/alpha/pid.xml" 2>/dev/null
check "$?" "alpha/pid.xml records alpha's worker pid"
if grep -q "$BETA_PID" "$STATE_DIR/projects/alpha/pid.xml" 2>/dev/null; then
  check 1 "alpha/pid.xml does NOT contain beta's worker pid"
else
  check 0 "alpha/pid.xml does NOT contain beta's worker pid"
fi

section "restart alpha leaves beta's process + on-disk state untouched"
# The STATE ISOLATION guarantee: restarting alpha must not touch beta's process
# or beta's own pid.xml. We assert against the OS (ps) and beta's on-disk file,
# not the aggregate snapshot: a separate supervisor restart-routing bug drops
# beta from the aggregate status after restart (tracked apart from isolation;
# see sysg-restart-drops-extra-projects). Beta's process + file are the truth.
sysg restart --project alpha >/dev/null 2>&1
echo "restart -p alpha rc: $?"
sleep 2

# beta's own process is still the same live pid
kill -0 "$BETA_PID" 2>/dev/null
check "$?" "beta/worker process ($BETA_PID) still alive after alpha restart"

# beta's pid file still records its original pid, unchanged
grep -q "$BETA_PID" "$STATE_DIR/projects/beta/pid.xml" 2>/dev/null
check "$?" "beta/pid.xml still records beta's original pid"

# alpha actually restarted: its pid file no longer names the old pid
if grep -q "$ALPHA_PID" "$STATE_DIR/projects/alpha/pid.xml" 2>/dev/null; then
  check 1 "alpha/pid.xml no longer records the pre-restart pid"
else
  check 0 "alpha/pid.xml no longer records the pre-restart pid"
fi

sysg stop --config "$CONFIG" >/dev/null 2>&1
finish
