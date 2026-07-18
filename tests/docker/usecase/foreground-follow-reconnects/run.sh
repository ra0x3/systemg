#!/usr/bin/env bash
# USE CASE: a foreground follow-stream is DEFENSIVE — it survives an interruption
# by reconnecting VISIBLY, never freezing silently while the service keeps going.
#
# WHAT THIS TESTS (real dogfooding bug: the stream froze mid-flight)
#   term1: `sysg start -c anchor.yaml` becomes the supervisor.
#   term2: `sysg start -c beta.yaml` attaches beta and streams its ticks.
#   Then `restart -p beta` from another shell BOUNCES beta — the follow stream
#   drops. A fragile stream would freeze here (the bug). The defensive stream must
#   announce the interruption and RECONNECT, resuming ticks from the new instance.
#
# EXPECTED OUTCOME
#   - beta ticks appear BEFORE the restart.
#   - after `restart -p beta`, the capture shows a 'reconnecting' notice AND
#     fresh BETA_TICK lines (the stream resumed, did not freeze).
set -u
. /usecase/lib.sh

B_OUT=/tmp/beta.out

section "term1: anchor becomes the supervisor"
python3 /usecase/fgcap.py /usecase/anchor.yaml 40 /tmp/anchor.out &
sleep 4

section "term2: beta attaches and streams (captured)"
python3 /usecase/fgcap.py /usecase/beta.yaml 34 "$B_OUT" &
sleep 7

TICKS_BEFORE="$(grep -c BETA_TICK "$B_OUT" 2>/dev/null || echo 0)"
echo "beta ticks before restart: $TICKS_BEFORE"
[ "$TICKS_BEFORE" -ge 1 ]
check "$?" "beta streamed ticks before the interruption"

section "restart -p beta from another shell — the stream must reconnect, not freeze"
sysg restart -p beta >/dev/null 2>&1
check "$?" "restart -p beta exits 0"
# Give the stream time to notice the drop, announce, reconnect, and resume.
sleep 12

echo "--- tail of beta.out ---"; tail -6 "$B_OUT" 2>/dev/null | tr -d '\r'
grep -qiE 'reconnect' "$B_OUT" 2>/dev/null
check "$?" "the capture announced a reconnect (interruption surfaced, not silent)"

# Resumed: there are MORE ticks now than right before the restart, proving the
# stream did not freeze at the interruption point.
TICKS_AFTER="$(grep -c BETA_TICK "$B_OUT" 2>/dev/null || echo 0)"
echo "beta ticks after reconnect: $TICKS_AFTER (before: $TICKS_BEFORE)"
[ "$TICKS_AFTER" -gt "$TICKS_BEFORE" ]
check "$?" "beta ticks RESUMED after the reconnect (stream did not freeze)"

sysg stop --supervisor >/dev/null 2>&1
finish
