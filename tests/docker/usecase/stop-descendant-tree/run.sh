#!/usr/bin/env bash
# USE CASE: stopping a service kills its WHOLE process tree.
#
# WHAT THIS TESTS
#   `forker` spawns background children (two `sleep 4000 &` under a shell). This
#   is the SG0001 PROCESS_TREE_ESCAPE class -- the #1 stop bug by fix count:
#   a service that leaves descendants running after stop. Stopping the service
#   must terminate the service AND every descendant; nothing may survive.
#
# EXPECTED OUTCOME
#   - After boot, the forker shell plus its child sleeps are alive (>= 3 procs
#     in the service's process group).
#   - `sysg stop -s forker` exits 0.
#   - NO process from the service's original process group survives (ps shows
#     zero). Any survivor is a process-tree escape and must fail the case.
#   - status shows forker stopped and its pid.xml entry is cleared.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
STATE_DIR="$HOME/.local/share/systemg"

section "boot the forking service"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3

STATUS="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
PID="$(unit_field "$STATUS" forker pid)"
echo "forker pid: $PID"
[ -n "$PID" ] && [ "$PID" != "absent" ] && [ "$PID" != "None" ]
check "$?" "forker has a pid"

# The process group led by the service; every descendant shares it.
PGID="$(ps -o pgid= -p "$PID" 2>/dev/null | tr -d ' ')"
echo "forker pgid: $PGID"
group_members() { ps -eo pgid= -o pid= | awk -v g="$PGID" '$1==g {print $2}' | wc -l | tr -d ' '; }
MEMBERS_BEFORE="$(group_members)"
echo "process group members before stop: $MEMBERS_BEFORE"
[ "$MEMBERS_BEFORE" -ge 3 ]
check "$?" "the service tree (shell + child sleeps) is alive before stop"

section "stop -s forker kills the entire tree"
sysg stop --service forker
check "$?" "stop -s forker exits 0"
sleep 2

MEMBERS_AFTER="$(group_members)"
echo "process group members after stop: $MEMBERS_AFTER"
[ "$MEMBERS_AFTER" = "0" ]
check "$?" "NO descendant of the service survives (no SG0001 escape)"

section "status and pid.xml agree the service is stopped"
STATUS2="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
[ "$(unit_field "$STATUS2" forker state)" != "running" ]
check "$?" "forker is no longer running in status"
if grep -q "<name>forker</name>" "$STATE_DIR/projects/demo/pid.xml" 2>/dev/null; then
  check 1 "forker's pid.xml entry was cleared"
else
  check 0 "forker's pid.xml entry was cleared"
fi

sysg stop --supervisor >/dev/null 2>&1
finish
