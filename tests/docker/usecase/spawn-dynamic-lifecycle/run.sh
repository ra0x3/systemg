#!/usr/bin/env bash
# USE CASE: dynamic children are authorized by `mode`, and die with their parent.
#
# WHAT THIS TESTS
#   Two failures the suite never exercised, because nothing covered dynamic
#   spawn end-to-end:
#     1. `spawn: {mode: dynamic}` with no `limits` block registered NO spawn
#        tree, so every child the unit asked for was refused. The documented
#        shorthand was dead config.
#     2. Dynamic children are forked by the SUPERVISOR, so they are not
#        descendants of the service pid and sat in the supervisor's own session
#        and process group -- the two the teardown sweep refuses to signal.
#        Stopping the parent left every worker running, reachable by nothing
#        short of a full supervisor shutdown.
#
# EXPECTED OUTCOME
#   - the orchestrator's three children are spawned and alive (mode alone is
#     enough to authorize spawning).
#   - each child leads its OWN session, not the supervisor's.
#   - `sysg stop -s orchestrator` leaves NO child alive.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot the orchestrator"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"

i=0
while [ "$i" -lt 30 ] && [ ! -f /tmp/spawned.done ]; do sleep 1; i=$((i+1)); done
[ -f /tmp/spawned.done ]
check "$?" "the orchestrator finished issuing its spawn requests"
cat /tmp/spawn.log 2>/dev/null || true

section "mode: dynamic alone authorizes spawning"
CHILD_PIDS="$(ps -eo pid=,args= | grep '[s]leep 3000' | awk '{print $1}')"
COUNT="$(echo "$CHILD_PIDS" | grep -c '[0-9]' || true)"
echo "live workers: $COUNT ($(echo $CHILD_PIDS | tr '\n' ' '))"
[ "$COUNT" -ge 3 ]
check "$?" "all three dynamic children are running (no tree = no spawn regression)"

section "each child leads its own session"
SUP_SID="$(ps -o sess= -p "$(cat "$HOME/.local/share/systemg/sysg.pid" 2>/dev/null || echo 1)" 2>/dev/null | tr -d ' ')"
SHARED=0
for p in $CHILD_PIDS; do
  SID="$(ps -o sess= -p "$p" 2>/dev/null | tr -d ' ')"
  if [ -n "$SUP_SID" ] && [ "$SID" = "$SUP_SID" ]; then
    echo "child $p shares the supervisor session $SID"
    SHARED=1
  fi
done
[ "$SHARED" = "0" ]
check "$?" "no child sits in the supervisor's session (it would be unkillable)"

section "stopping the parent reclaims every child"
sysg stop --config "$CONFIG" --service orchestrator
check "$?" "stop -s orchestrator exits 0"
sleep 3

# Guard against a vacuous pass: with no children spawned there is nothing to
# survive, and the sweep would look correct while being untested.
[ "$COUNT" -ge 3 ]
check "$?" "there were children to reclaim in the first place"

SURVIVORS=0
for p in $CHILD_PIDS; do
  if kill -0 "$p" 2>/dev/null; then
    echo "SURVIVOR: dynamic child $p still alive after parent stop"
    SURVIVORS=$((SURVIVORS+1))
  fi
done
[ "$SURVIVORS" = "0" ]
check "$?" "NO dynamic child survives its parent's stop"

sysg stop --config "$CONFIG" >/dev/null 2>&1
finish
