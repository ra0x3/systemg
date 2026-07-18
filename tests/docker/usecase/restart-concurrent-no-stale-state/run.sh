#!/usr/bin/env bash
# USE CASE: concurrent restarts never leave a unit stuck at `lost`.
#
# WHAT THIS TESTS (the hardest real bug found on the live stack)
#   Three `restart -p` commands fired at once left units reporting `lost`/`warn`
#   FOREVER — nothing could clear them, and the project sat at WARN until the
#   supervisor was stopped and its state deleted by hand.
#
#   The mechanism took five bad guesses to find, so it is worth stating exactly:
#
#     - `probe_service_state` is the ONLY place that witnesses a service exit:
#       it reaps the child and clears the pid from pid.xml.
#     - It did NOT write the lifecycle record; it returned `Exited(status)` and
#       left that to the caller.
#     - Under concurrent restarts the caller is a RACING restart that discards
#       the result — so nothing ever superseded the earlier `mark_running`.
#     - state.xml was left saying `running` with a pid that no longer existed.
#     - Status reads the pid from state.xml FIRST (pid.xml is only a fallback),
#       finds it dead, and derives `Missing` -> `Lost`.
#
#   So pid.xml looked perfectly clean while state.xml was the liar. Every fix
#   aimed at pid.xml (clearing stale pids, sweeping orphaned process groups)
#   was correct in itself and changed nothing here.
#
#   The fix records the exit AT THE POINT OF OBSERVATION, plus a monitor-tick
#   sweep that corrects any `running` entry whose pid is verifiably dead — so no
#   future race can reintroduce a `lost`-forever unit.
#
# EXPECTED OUTCOME
#   After three concurrent restarts:
#     - no unit is `lost`;
#     - no state entry claims `running` with a dead pid;
#     - the completed one-shot is `done` (not `stopped`, which would read as a
#       failed dependency downstream).
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "start the project"
sysg start --config "$CONFIG" --daemonize >/dev/null 2>&1
check "$?" "project started"
sleep 5

S0="$(sysg status --format json 2>/dev/null)"
[ "$(unit_field "$S0" worker state)" = "running" ]
check "$?" "worker is running (baseline)"
[ "$(unit_field "$S0" build state)" = "done" ]
check "$?" "build completed (baseline)"

section "fire three restarts CONCURRENTLY"
sysg restart -p churn >/dev/null 2>&1 &
sysg restart -p churn >/dev/null 2>&1 &
sysg restart -p churn >/dev/null 2>&1 &
wait
echo "all three restarts returned"
sleep 12

echo "--- status after the race ---"
sysg status 2>/dev/null | head -25

section "no unit may be stuck at lost"
S1="$(sysg status --format json 2>/dev/null)"
LOST="$(printf '%s' "$S1" | python3 -c '
import json,sys
d=json.load(sys.stdin)
print(",".join(u["name"] for u in d.get("units",[]) if u.get("state")=="lost"))' 2>/dev/null)"
echo "lost units: [${LOST}]"
[ -z "$LOST" ]
check "$?" "NO unit is reporting lost"

section "no state entry claims running with a dead pid"
STALE="$(printf '%s' "$S1" | python3 -c '
import json,subprocess,sys
d=json.load(sys.stdin)
bad=[]
for u in d.get("units",[]):
    p=(u.get("process") or {})
    pid=p.get("pid")
    if u.get("state")=="running" and pid:
        if subprocess.run(["kill","-0",str(pid)],capture_output=True).returncode!=0:
            bad.append(u["name"])
print(",".join(bad))' 2>/dev/null)"
echo "running-but-dead: [${STALE}]"
[ -z "$STALE" ]
check "$?" "every running unit maps to a live process"

section "the completed one-shot stays done"
BUILD="$(unit_field "$S1" build state)"
echo "build state: $BUILD"
[ "$BUILD" = "done" ]
check "$?" "build is done (not stopped, which reads as a failed dependency)"

section "the project settles healthy"
sleep 8
OVERALL="$(sysg status --format json 2>/dev/null | python3 -c '
import json,sys; print(json.load(sys.stdin).get("overall_health",""))' 2>/dev/null)"
echo "overall: $OVERALL"
[ "$OVERALL" = "healthy" ]
check "$?" "overall health is healthy after the race"

sysg stop --supervisor >/dev/null 2>&1
finish
