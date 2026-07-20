#!/usr/bin/env bash
# USE CASE: a foreground follow stream survives a project restart without
# freezing or replaying historical output.
#
# WHAT THIS TESTS (real dogfooding bug: the stream froze mid-flight)
#   term1: `sysg start -c anchor.yaml` becomes the supervisor.
#   term2: `sysg start -c beta.yaml` attaches beta and streams its ticks.
#   Then `restart -p beta` from another shell bounces beta. The multiplexed
#   follow remains attached across process replacement and streams the new
#   instance without replaying historical output.
#
# EXPECTED OUTCOME
#   - beta ticks appear BEFORE the restart.
#   - after `restart -p beta`, fresh BETA_TICK lines continue.
#   - each one-shot runs exactly once per explicit project start.
set -u
. /usecase/lib.sh

B_OUT=/tmp/beta.out

section "term1: anchor becomes the supervisor"
python3 /usecase/fgcap.py /usecase/anchor.yaml 70 /tmp/anchor.out &
sleep 4

section "term2: beta attaches and streams (captured)"
python3 /usecase/fgcap.py /usecase/beta.yaml 64 "$B_OUT" &
sleep 7

TICKS_BEFORE="$(grep -c BETA_TICK "$B_OUT" 2>/dev/null || echo 0)"
BUILD_BEFORE="$(grep -c BETA_BUILD_DONE "$B_OUT" 2>/dev/null || echo 0)"
PROBE_BEFORE="$(grep -c BETA_PROBE_PONG "$B_OUT" 2>/dev/null || echo 0)"
echo "beta ticks before restart: $TICKS_BEFORE"
[ "$TICKS_BEFORE" -ge 1 ] && [ "$BUILD_BEFORE" = "1" ] && [ "$PROBE_BEFORE" = "1" ]
check "$?" "beta streamed ticks and each one-shot once before restart"

section "restart -p beta from another shell — the stream must continue"
sysg restart -p beta >/dev/null 2>&1
check "$?" "restart -p beta exits 0"
# Give the multiplexed stream time to follow the replacement processes.
sleep 12

echo "--- tail of beta.out ---"; tail -6 "$B_OUT" 2>/dev/null | tr -d '\r'
TICKS_AFTER="$(grep -c BETA_TICK "$B_OUT" 2>/dev/null || echo 0)"
echo "beta ticks after restart: $TICKS_AFTER (before: $TICKS_BEFORE)"
[ "$TICKS_AFTER" -gt "$TICKS_BEFORE" ]
check "$?" "beta ticks continue through the restart"

section "a live stream must not replay STATIC history"
# The reconnect path fell back to rendering a full status-grouped snapshot,
# which prints an "Offline Services" section and dumps the finished output of
# every completed one-shot into a LIVE terminal. Observed on a real project:
# a foreground stream suddenly printed the build/migrations/redis history it had
# already shown at boot, then died. A follow tails; it never replays.
! grep -qi 'Offline Services' "$B_OUT"
check "$?" "no 'Offline Services' block was dumped into the live stream"

BUILD_ECHOES="$(grep -c BETA_BUILD_DONE "$B_OUT" 2>/dev/null | tr -d ' \n')"
PROBE_ECHOES="$(grep -c BETA_PROBE_PONG "$B_OUT" 2>/dev/null | tr -d ' \n')"
BUILD_ECHOES="${BUILD_ECHOES:-0}"; PROBE_ECHOES="${PROBE_ECHOES:-0}"
echo "one-shot executions: build=$BUILD_ECHOES probe=$PROBE_ECHOES"
if [ "$BUILD_ECHOES" = "$((BUILD_BEFORE + 1))" ] && [ "$PROBE_ECHOES" = "$((PROBE_BEFORE + 1))" ]; then RC=0; else RC=1; fi
check "$RC" "explicit restart reran each one-shot exactly once without replay"

section "the stream is still ALIVE at the end"
# The real failure ended with the stream dead: a static dump, then
# 'log stream ended unexpectedly'. Reconnecting is only correct if it KEEPS
# streaming afterwards.
BEFORE_FINAL="$(grep -c BETA_TICK "$B_OUT" 2>/dev/null || echo 0)"
sleep 6
AFTER_FINAL="$(grep -c BETA_TICK "$B_OUT" 2>/dev/null || echo 0)"
echo "ticks still arriving: $BEFORE_FINAL -> $AFTER_FINAL"
[ "$AFTER_FINAL" -gt "$BEFORE_FINAL" ]
check "$?" "the stream is STILL alive and tailing at the end"

sysg stop --supervisor >/dev/null 2>&1
finish
