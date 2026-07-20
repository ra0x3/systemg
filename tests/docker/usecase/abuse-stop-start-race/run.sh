#!/usr/bin/env bash
# ABUSE: race `stop --supervisor` against `start --daemonize` in a tight loop.
#
# WHAT THIS ABUSES
#   This is the BUG-4 prod pain weaponized: rapidly tear the supervisor down and
#   bring it back, sometimes overlapping, so a `start` can land on a supervisor
#   that is mid-shutdown and a `stop` can land on one mid-boot. A naive
#   liveness check (stale pidfile) would route a command into a dying daemon and
#   hang, or leave a wedged runtime that needs `sysg purge`. The SupervisorHealth
#   preflight must keep this converging: after the churn, ONE clean supervisor
#   (or none), never a wedged half-state, and a final start must succeed.
#
# HARD INVARIANTS
#   - no invocation HANGS (each bounded by `timeout`),
#   - after the churn, a final `start` yields a supervisor that answers and runs
#     both services,
#   - exactly ONE supervisor pid, exactly 2 service processes, ps == status.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
STATE_DIR="$HOME/.local/share/systemg"
ROUNDS=8
TIMEOUT=30

section "boot once"
sysg start --config "$CONFIG" --daemonize
check "$?" "initial start exits 0"
sleep 2

section "$ROUNDS rounds of overlapping stop --supervisor / start --daemonize"
HANG=0
i=0
while [ "$i" -lt "$ROUNDS" ]; do
  timeout "$TIMEOUT" sysg stop --supervisor >/dev/null 2>&1 &
  SP=$!
  timeout "$TIMEOUT" sysg start --config "$CONFIG" --daemonize >/dev/null 2>&1 &
  ST=$!
  wait "$SP"; SP_RC=$?
  wait "$ST"; ST_RC=$?
  echo "round $((i+1)): stop=$SP_RC start=$ST_RC"
  [ "$SP_RC" = "124" ] && HANG=$((HANG+1))
  [ "$ST_RC" = "124" ] && HANG=$((HANG+1))
  i=$((i+1))
done
echo "hung invocations: $HANG"
[ "$HANG" = "0" ]
check "$?" "no stop/start invocation hung during the race"

section "quiesce, then a clean final start must win"
timeout "$TIMEOUT" sysg stop --supervisor >/dev/null 2>&1
[ "$?" != "124" ]
check "$?" "final stop --supervisor did not hang"
sleep 1
timeout "$TIMEOUT" sysg start --config "$CONFIG" --daemonize >/tmp/fs.err 2>&1
RC=$?
cat /tmp/fs.err
[ "$RC" != "124" ]
check "$?" "final start did not hang"
[ "$RC" = "0" ]
check "$?" "final start exits 0 (no wedged runtime blocking it)"
sleep 3

section "exactly one clean supervisor + both services, ps == status"
[ -e "$STATE_DIR/sysg.pid" ]
check "$?" "supervisor pid file present"
SUPS="$(pgrep -f 'sysg' | grep -v $$ | wc -l | tr -d ' ')"
echo "sysg-ish processes: $SUPS"
S="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
grep -qi "No running supervisor" <<<"$S" && DEAD=1 || DEAD=0
[ "$DEAD" = "0" ]
check "$?" "supervisor answers status after the race"
RUN=0
for svc in web api; do
  P="$(unit_field "$S" "$svc" pid)"
  [ -n "$P" ] && [ "$P" != "absent" ] && pid_alive "$P" && RUN=$((RUN+1))
done
[ "$RUN" = "2" ]
check "$?" "both services running on live pids after the race"
NOW_SLEEPS="$(pgrep -c -x sleep || echo 0)"
[ "$NOW_SLEEPS" = "2" ]
check "$?" "exactly 2 service processes (no orphans from the churn)"

sysg stop --supervisor >/dev/null 2>&1
finish
