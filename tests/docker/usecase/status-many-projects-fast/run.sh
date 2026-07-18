#!/usr/bin/env bash
# USE CASE: status is fast AND accurate across MANY projects and configs.
#
# WHAT THIS TESTS
#   The scale/accuracy stress: 4 config files, 10 projects each, 3 services per
#   project = 40 projects / 120 units, all under one supervisor (registered via 4
#   separate `start -c` calls). status must (a) report EVERY one of the 120 units
#   accurately (ps == status, no dropped project/service at scale) and (b) return
#   quickly — both the cached path and the --live re-probe path under a ceiling.
#   A status that goes O(n^2) or serial-forks per unit would blow the budget.
#
# EXPECTED OUTCOME
#   - 4 configs x 10 projects x 3 services boot into one supervisor.
#   - status lists all 120 units; every unit is running on an alive pid.
#   - cached `status --format json` returns under CACHED_CEIL_MS.
#   - `status --live --format json` returns under LIVE_CEIL_MS.
#
#   NOTE: RED until the status-cache staleness fix lands. At scale the cached
#   snapshot is built in configured mode and reports live processes as
#   `stopped` (ps != status). The accuracy checks below pin that bug.
set -u
. /usecase/lib.sh

CONFIGS=4
PROJECTS_PER=10
SVCS_PER=3
EXPECT=$((CONFIGS * PROJECTS_PER * SVCS_PER))   # 120
CACHED_CEIL_MS=3000
LIVE_CEIL_MS=6000

section "generate $CONFIGS configs x $PROJECTS_PER projects x $SVCS_PER services"
mkdir -p /usecase/gen
ci=0
while [ "$ci" -lt "$CONFIGS" ]; do
  f="/usecase/gen/stack_$ci.yaml"
  echo 'version: "2"' > "$f"
  echo 'projects:' >> "$f"
  pi=0
  while [ "$pi" -lt "$PROJECTS_PER" ]; do
    pid="c${ci}p${pi}"
    echo "  ${pid}:" >> "$f"
    echo "    name: ${pid}" >> "$f"
    echo "    services:" >> "$f"
    si=0
    while [ "$si" -lt "$SVCS_PER" ]; do
      echo "      svc${si}:" >> "$f"
      echo "        command: \"sleep 3000\"" >> "$f"
      si=$((si+1))
    done
    pi=$((pi+1))
  done
  ci=$((ci+1))
done
ls /usecase/gen
check "$?" "configs generated"

section "boot all 4 configs into one supervisor"
ci=0
while [ "$ci" -lt "$CONFIGS" ]; do
  sysg start --config "/usecase/gen/stack_$ci.yaml" --daemonize >/dev/null 2>&1
  check "$?" "start config $ci exits 0"
  ci=$((ci+1))
done
sleep 5

section "accuracy: status reports all $EXPECT units, ps agrees"
S="$(sysg status --format json 2>/dev/null)"
COUNT="$(unit_count "$S")"
echo "status unit count: $COUNT (expected $EXPECT)"
[ "$COUNT" = "$EXPECT" ]
check "$?" "status lists exactly $EXPECT units (no drops at scale)"

# Count how many of the reported units are actually alive in ps.
ALIVE="$(python3 - "$S" <<'PY'
import json,sys,os
snap=json.loads(sys.argv[1])
alive=0
for u in snap.get("units",[]):
    p=(u.get("process") or {}).get("pid")
    if not p: continue
    try:
        os.kill(int(p),0); alive+=1
    except OSError: pass
print(alive)
PY
)"
echo "alive pids seen by ps: $ALIVE"
[ "$ALIVE" = "$EXPECT" ]
check "$?" "ps confirms all $EXPECT reported pids are alive (ps == status)"

section "latency: cached status returns under ${CACHED_CEIL_MS}ms"
T0=$(date +%s%3N)
sysg status --format json >/dev/null 2>&1
T1=$(date +%s%3N)
CACHED_MS=$((T1 - T0))
echo "cached status latency: ${CACHED_MS}ms (ceiling ${CACHED_CEIL_MS}ms)"
[ "$CACHED_MS" -lt "$CACHED_CEIL_MS" ]
check "$?" "cached status under ${CACHED_CEIL_MS}ms"

section "latency: --live status returns under ${LIVE_CEIL_MS}ms"
T0=$(date +%s%3N)
sysg status --live --format json >/dev/null 2>&1
T1=$(date +%s%3N)
LIVE_MS=$((T1 - T0))
echo "live status latency: ${LIVE_MS}ms (ceiling ${LIVE_CEIL_MS}ms)"
[ "$LIVE_MS" -lt "$LIVE_CEIL_MS" ]
check "$?" "live status under ${LIVE_CEIL_MS}ms"

sysg stop --supervisor >/dev/null 2>&1
finish
