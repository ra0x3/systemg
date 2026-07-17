#!/usr/bin/env bash
# USE CASE: an EXTERNAL crash of a forking service leaves NO orphans and the
# service returns to a KNOWN state -- never resting on `Lost`.
#
# WHAT THIS TESTS
#   The crash path is distinct from `sysg stop`. Here we `kill -9` the tracked
#   LEADER only (simulating an external crash / OOM / segfault). The leader's
#   forked children (two `sleep 4000 &`) are reparented to init but keep the
#   leader's process group. sysg must, before respawning per restart_policy:
#     1. reap the whole ORPHANED group (no survivor holds resources), and
#     2. respawn the service on a NEW pid in a fresh group,
#   so status transitions crash -> respawned and NEVER rests on `Lost`.
#   A survivor from the old group is the crash-path leak this fix closes.
#
# EXPECTED OUTCOME
#   - Before the kill, the service group has >= 3 procs (shell + 2 sleeps).
#   - After `kill -9 <leader>`, sysg respawns forker on a NEW pid.
#   - ZERO members of the ORIGINAL process group survive.
#   - status ends with forker running (not `lost`, not `failed`).
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot the forking service"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3

STATUS="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
LEADER="$(unit_field "$STATUS" forker pid)"
echo "forker leader pid: $LEADER"
[ -n "$LEADER" ] && [ "$LEADER" != "absent" ] && [ "$LEADER" != "None" ]
check "$?" "forker has a leader pid"

OLD_PGID="$(ps -o pgid= -p "$LEADER" 2>/dev/null | tr -d ' ')"
echo "forker original pgid: $OLD_PGID"
group_members() { ps -eo pgid= -o pid= | awk -v g="$OLD_PGID" '$1==g {print $2}' | wc -l | tr -d ' '; }
MEMBERS_BEFORE="$(group_members)"
echo "original group members before crash: $MEMBERS_BEFORE"
[ "$MEMBERS_BEFORE" -ge 3 ]
check "$?" "the service tree (shell + child sleeps) is alive before the crash"

section "external crash: kill -9 the leader ONLY"
kill -9 "$LEADER" 2>/dev/null
echo "sent SIGKILL to leader $LEADER; the child sleeps keep pgid $OLD_PGID and reparent to init"

section "sysg reaps the orphaned group and respawns on a new pid"
RESPAWN=0
i=0
while [ "$i" -lt 25 ]; do
  sleep 1
  STATUS2="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
  NEW="$(unit_field "$STATUS2" forker pid)"
  if [ -n "$NEW" ] && [ "$NEW" != "absent" ] && [ "$NEW" != "None" ] && [ "$NEW" != "$LEADER" ] && pid_alive "$NEW"; then
    RESPAWN=1; echo "forker respawned on pid $NEW"; break
  fi
  i=$((i+1))
done
[ "$RESPAWN" = "1" ]
check "$?" "sysg respawned forker on a NEW pid after the crash"

MEMBERS_AFTER="$(group_members)"
echo "original group members after respawn: $MEMBERS_AFTER"
[ "$MEMBERS_AFTER" = "0" ]
check "$?" "NO member of the original crashed group survives (no crash-path orphan leak)"

section "status rests on a KNOWN state, never on 'lost'"
FINAL_STATE="$(unit_field "$STATUS2" forker state)"
echo "final forker state: $FINAL_STATE"
[ "$FINAL_STATE" != "lost" ]
check "$?" "forker did not rest on 'lost' after the crash"
[ "$FINAL_STATE" = "running" ]
check "$?" "forker is running again on the fresh group"

sysg stop --supervisor >/dev/null 2>&1
finish
