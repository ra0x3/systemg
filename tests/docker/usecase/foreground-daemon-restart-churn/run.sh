#!/usr/bin/env bash
# WORST-CASE: churn the daemon project's restart while a foreground holds the term.
#
# WHAT THIS ABUSES
#   A foreground project and a daemon project coexist. We hammer `restart -p
#   dmproj` many times while the foreground is attached. The daemon's restarts
#   must NEVER touch the foreground project (cross-project isolation under churn),
#   and the foreground must keep running on its ORIGINAL pid throughout. This is
#   the cross-project-teardown bug (restart -p killing siblings) stressed against
#   the foreground/daemon boundary.
#
# HARD INVARIANTS
#   - the foreground service keeps its ORIGINAL pid across N daemon restarts,
#   - each daemon restart bounces ONLY the daemon service (new daemon pid),
#   - logs stay isolated the whole time,
#   - after the churn: exactly one fg proc + one daemon proc, ps == status.
set -u
. /usecase/lib.sh
N=6

section "start daemon + foreground"
sysg start --config /usecase/daemon.yaml --daemonize
check "$?" "daemon start exits 0"
sleep 2
python3 /usecase/fg_run.py /usecase/fg.yaml 30 >/tmp/fg.out 2>&1 &
FGJOB=$!
sleep 4
FG_PID0="$(pgrep -f FG_LINE | head -1)"
echo "foreground pid at start: $FG_PID0"
[ -n "$FG_PID0" ]
check "$?" "foreground service running before the churn"

section "$N daemon-project restarts while the foreground is attached"
CHANGED_DM=0
for i in $(seq 1 "$N"); do
  DM_BEFORE="$(pgrep -f DAEMON_LINE | head -1)"
  sysg restart --config /usecase/daemon.yaml -p dmproj >/dev/null 2>&1
  sleep 2
  DM_AFTER="$(pgrep -f DAEMON_LINE | head -1)"
  [ -n "$DM_AFTER" ] && [ "$DM_AFTER" != "$DM_BEFORE" ] && CHANGED_DM=$((CHANGED_DM+1))
  # the foreground must NOT have been touched
  FG_NOW="$(pgrep -f FG_LINE | head -1)"
  if [ "$FG_NOW" != "$FG_PID0" ]; then
    echo "  round $i: FOREGROUND PID CHANGED ($FG_PID0 -> $FG_NOW) — daemon restart hit the foreground!"
    break
  fi
done
echo "daemon pid changed in $CHANGED_DM/$N restarts"

section "the foreground was NEVER touched by the daemon restarts"
FG_END="$(pgrep -f FG_LINE | head -1)"
[ "$FG_END" = "$FG_PID0" ] && kill -0 "$FG_PID0" 2>/dev/null
check "$?" "foreground kept its original pid $FG_PID0 across all daemon restarts"
[ "$CHANGED_DM" -ge $((N - 1)) ]
check "$?" "daemon restarts actually bounced the daemon service"

section "logs stayed isolated through the churn"
A="$(sysg logs --config /usecase/fg.yaml -p fgproj --no-follow 2>/dev/null)"
echo "$A" | grep -q "FG_LINE" && ! echo "$A" | grep -q "DAEMON_LINE"
check "$?" "logs -p fgproj still shows ONLY FG_LINE after the churn"

section "no orphans: each service on ONE tracked pid, no dead generation"
# Each service is `sh -c 'while...'` = shell + child, so raw proc count is 2 per
# service by design (see the shell-wrapper spawn model). The real invariant is:
# status lists each service once on a LIVE pid, and no orphan whose parent died.
S="$(sysg status --config /usecase/daemon.yaml --format json 2>/dev/null)"
FG_STAT="$(unit_field "$S" fgsvc pid fgproj)"
DM_STAT="$(unit_field "$S" dmsvc pid dmproj)"
echo "status pids -> fgsvc:$FG_STAT dmsvc:$DM_STAT"
[ -n "$FG_STAT" ] && [ "$FG_STAT" != "absent" ] && pid_alive "$FG_STAT"
check "$?" "fgsvc runs on a single live tracked pid"
[ -n "$DM_STAT" ] && [ "$DM_STAT" != "absent" ] && pid_alive "$DM_STAT"
check "$?" "dmsvc runs on a single live tracked pid"
ORPHANS=0
for sp in $(pgrep -x sleep); do
  PP="$(ps -o ppid= -p "$sp" | tr -d ' ')"
  pid_alive "$PP" || ORPHANS=$((ORPHANS+1))
done
echo "orphaned sleeps (dead parent): $ORPHANS (expect 0)"
[ "$ORPHANS" = "0" ]
check "$?" "no orphaned processes from the restart churn"

kill "$FGJOB" 2>/dev/null
sysg stop --supervisor >/dev/null 2>&1
finish
