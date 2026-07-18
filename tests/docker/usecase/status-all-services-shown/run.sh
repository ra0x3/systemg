#!/usr/bin/env bash
# USE CASE: status shows ALL N services of a project, and ps agrees with status.
#
# WHAT THIS TESTS
#   The "status drops services" bug: a single project with N same-command
#   services used to collapse to ONE row because state was keyed by config hash
#   (same command => same hash => collision). With the composite state key
#   {version}:{project}:{service} every service is distinct. This asserts the
#   ground truth of the whole rebuild: what `ps` shows and what `sysg status`
#   shows MUST agree, for every one of the N services.
#
# EXPECTED OUTCOME
#   - Boot demo with 5 identical-command services.
#   - status lists all 5 units, each running on a distinct, alive pid.
#   - the count of alive service pids in ps == the count of running units in
#     status == 5. No collisions, no drops.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
SERVICES="alpha bravo charlie delta echo"

section "boot the 5-service project"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3

section "status shows all 5 services, each on a distinct alive pid"
S="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
COUNT="$(unit_count "$S")"
echo "status unit count: $COUNT"
[ "$COUNT" = "5" ]
check "$?" "status lists exactly 5 units (no collision/drop)"

PIDS=""
ALL_ALIVE=1
for svc in $SERVICES; do
  P="$(unit_field "$S" "$svc" pid)"
  ST="$(unit_field "$S" "$svc" state)"
  echo "  $svc -> pid:$P state:$ST"
  [ -n "$P" ] && [ "$P" != "absent" ] && pid_alive "$P" && [ "$ST" = "running" ] || ALL_ALIVE=0
  PIDS="$PIDS $P"
done
[ "$ALL_ALIVE" = "1" ]
check "$?" "every one of the 5 services is running on an alive pid"

DISTINCT="$(echo $PIDS | tr ' ' '\n' | sort -u | grep -c .)"
[ "$DISTINCT" = "5" ]
check "$?" "all 5 pids are distinct (no shared/colliding row)"

section "ps agrees with status (ground truth)"
PS_ALIVE=0
for p in $PIDS; do pid_alive "$p" && PS_ALIVE=$((PS_ALIVE+1)); done
echo "ps sees $PS_ALIVE alive service pids; status reported $COUNT units"
[ "$PS_ALIVE" = "5" ] && [ "$COUNT" = "5" ]
check "$?" "ps count == status count == 5"

sysg stop --supervisor >/dev/null 2>&1
finish
