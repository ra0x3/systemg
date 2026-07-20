#!/usr/bin/env bash
# ABUSE: churn one service through many restart generations back-to-back.
#
# WHAT THIS ABUSES
#   A single service is restarted over and over with almost no gap, so each new
#   generation is spawned while the previous one may still be tearing down. This
#   hammers the teardown/respawn edge: the old pid must be reaped before the new
#   one is recorded, or the supervisor leaks a generation. Across N restarts a
#   leak shows up as growing `sleep` count, a stale pid in status that no longer
#   matches ps, or a zombie left behind. The pid MUST change each generation
#   (proving a real restart, not a no-op) yet only ONE live pid may exist per
#   service at any settle point.
#
# HARD INVARIANTS
#   - each restart exits 0 and does not hang,
#   - web's pid CHANGES across generations (real restarts, not no-ops),
#   - after the churn: exactly 2 `sleep` procs, no zombies,
#   - status pid == ps pid for both services (no stale generation leaked).
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
N=15

section "boot the stack"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 2
[ "$(pgrep -c -x sleep || echo 0)" = "2" ]
check "$?" "exactly 2 service processes before the churn"

section "churn web through $N tight restart generations"
S="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
PREV="$(unit_field "$S" web pid demo)"
CHANGES=0
HANG=0
FAIL=0
i=0
while [ "$i" -lt "$N" ]; do
  timeout 20 sysg restart --config "$CONFIG" -s web >/dev/null 2>&1
  RC=$?
  [ "$RC" = "124" ] && HANG=$((HANG+1))
  [ "$RC" != "0" ] && [ "$RC" != "124" ] && FAIL=$((FAIL+1))
  S="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
  CUR="$(unit_field "$S" web pid demo)"
  [ -n "$CUR" ] && [ "$CUR" != "absent" ] && [ "$CUR" != "$PREV" ] && CHANGES=$((CHANGES+1))
  PREV="$CUR"
  i=$((i+1))
done
echo "hung: $HANG  failed: $FAIL  pid-changes: $CHANGES/$N"
[ "$HANG" = "0" ]
check "$?" "no restart in the churn hung"
[ "$FAIL" = "0" ]
check "$?" "every restart in the churn exited 0"
[ "$CHANGES" = "$N" ]
check "$?" "web got a fresh pid on every generation (real restarts, no no-ops)"

section "no generation leaked: exactly 2 procs, no zombies, ps == status"
sleep 2
NOW_SLEEPS="$(pgrep -c -x sleep || echo 0)"
echo "sleep procs after churn: $NOW_SLEEPS (expected 2)"
[ "$NOW_SLEEPS" = "2" ]
check "$?" "exactly 2 service processes (no leaked generation)"
ZOMBIES="$(ps -eo stat= 2>/dev/null | grep -c '^Z')"
[ -n "$ZOMBIES" ] || ZOMBIES=0
echo "zombies: $ZOMBIES"
[ "$ZOMBIES" -eq 0 ]
check "$?" "no zombie processes left by the churn"

S="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
MATCH=0
for svc in web api; do
  P="$(unit_field "$S" "$svc" pid demo)"
  echo "  $svc -> status pid:$P"
  [ -n "$P" ] && [ "$P" != "absent" ] && pid_alive "$P" && MATCH=$((MATCH+1))
done
[ "$MATCH" = "2" ]
check "$?" "status pid is live for both services (no stale pid leaked)"

sysg stop --supervisor >/dev/null 2>&1
finish
