#!/usr/bin/env bash
# USE CASE: a dependency that goes UNHEALTHY takes its dependents down with it.
#
# WHAT THIS TESTS
#   A crashing dependency already cascades (crash-dependent-selfheal covers it).
#   A dependency that stays ALIVE but starts FAILING ITS HEALTH CHECK did not:
#   the periodic probe stopped the unit through a path that removed its child
#   handle and reaped it directly, so the monitor never observed an exit, the
#   unit never landed in `failed_services`, and `stop_dependents` never ran.
#   Dependents kept running against a dependency that had already been declared
#   unhealthy — the "browser still pointed at a display that failed its probe"
#   class, which looks healthy from every metric while producing nothing.
#
# EXPECTED OUTCOME
#   - boot: display and browser both up.
#   - the probe endpoint starts returning 503 -> display is stopped by the sweep
#     AND browser is stopped as a casualty of its dependency.
#   - the endpoint recovers -> display comes back and browser revives on a NEW
#     pid, with no operator intervention.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
pid_of() { unit_field "$1" "$2" pid stream; }
is_up()   { [ -n "$1" ] && [ "$1" != "absent" ] && [ "$1" != "None" ] && pid_alive "$1"; }
is_down() { [ -z "$1" ] || [ "$1" = "absent" ] || [ "$1" = "None" ] || ! pid_alive "$1"; }

section "boot the stack"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 5

S="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
DISP0="$(pid_of "$S" display)"; BROW0="$(pid_of "$S" browser)"
echo "boot: display=$DISP0 browser=$BROW0"
is_up "$DISP0" && is_up "$BROW0"
check "$?" "display and browser are both up"

section "the display goes unhealthy while STAYING ALIVE"
rm -f /tmp/display.ok
echo "probe endpoint now returns 503; display's process is still running"

# The periodic sweep runs on its own timer, so allow well past one interval.
BROW_DOWN=1; i=0
while [ "$i" -lt 90 ]; do
  sleep 2
  S="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
  B="$(pid_of "$S" browser)"
  if is_down "$B"; then
    echo "browser went down at ~$((i*2))s"
    BROW_DOWN=0
    break
  fi
  i=$((i+1))
done
check "$BROW_DOWN" "browser is stopped as a casualty of its unhealthy dependency"

section "the display recovers — the stack heals itself"
touch /tmp/display.ok
HEALED=1; i=0
while [ "$i" -lt 90 ]; do
  sleep 2
  S="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
  D="$(pid_of "$S" display)"; B="$(pid_of "$S" browser)"
  if is_up "$D" && is_up "$B" && [ "$B" != "$BROW0" ]; then
    echo "healed at ~$((i*2))s: display=$D browser=$B"
    HEALED=0
    break
  fi
  i=$((i+1))
done
check "$HEALED" "browser revives on a new pid once the dependency is healthy again"

sysg stop --config "$CONFIG" >/dev/null 2>&1
finish
