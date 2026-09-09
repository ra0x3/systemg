#!/usr/bin/env bash
# USE CASE: a readiness probe dies with the process it was launched for.
#
# WHAT THIS TESTS
#   The production sequence, reproduced: a command restart opens a readiness
#   probe whose health check does not pass; that process exits on its own and
#   restart_policy respawns it, which opens a SECOND probe. The first probe was
#   started for a process that no longer exists, but it knows its subject only
#   by service name, so it keeps probing the replacement. When the health check
#   finally passes, BOTH probes report the same unit ready and both write the
#   same `health check (attempt N, Xs)` progress row, which is why the rendered
#   attempt counter flips between two counters.
#
# EXPECTED OUTCOME
#   - The restart and the respawn each open a probe, so two probes overlap.
#   - Exactly ONE probe passes after the restart: the one whose process is still
#     the unit's current generation. The superseded probe abandons itself
#     instead of reporting on a process it never started. Zero passes would mean
#     readiness itself broke, so the count must be exactly one.
#
# The health check is made to pass once the SECOND probe has opened, not after a
# fixed delay. The unit lives 12s per generation and the restart backoff grows
# with the attempt count, so a fixed delay lands inside a live generation or
# just after one died depending on how many respawns happened to run first.
set -u
. /usecase/lib.sh

touch /tmp/healthy
section "boot: the health check passes once so start completes"
sysg start --config /usecase/stack.yaml --daemonize
check "$?" "start exits 0"
sleep 3

section "restart with a failing health check, then let the unit exit under it"
rm -f /tmp/healthy
sysg restart -s web >/tmp/restart.log 2>&1 &
RESTART=$!

probes_open() {
  sysg logs --supervisor 2>/dev/null \
    | sed -n '/Performing immediate restart for service: web/,$p' \
    | grep -c "Waiting for health check of 'web'"
}

section "make the health check pass while both probes are in flight"
WAITED=0
until [ "$(probes_open)" -ge 2 ] || [ "$WAITED" -ge 90 ]; do
  sleep 1
  WAITED=$((WAITED + 1))
done
echo "second probe opened after ${WAITED}s"
touch /tmp/healthy
sleep 8

LOG="$(sysg logs --supervisor 2>/dev/null)"
AFTER="$(echo "$LOG" | sed -n '/Performing immediate restart for service: web/,$p')"
STARTED="$(echo "$AFTER" | grep -c "Waiting for health check of 'web'")"
PASSED="$(echo "$AFTER" | grep -c "Health check passed for 'web'")"
echo "after the restart -> probes opened: $STARTED   probes passed: $PASSED"

[ "$STARTED" -ge 2 ]
check "$?" "the restart and the respawn both opened a probe"

[ "$PASSED" -eq 1 ]
check "$?" "exactly one probe reported the unit ready (the current generation's)"

section "the restart command itself"
wait "$RESTART"
RESTART_RC=$?
echo "restart exit code: $RESTART_RC"
cat /tmp/restart.log
# The health check was failing for the whole readiness window, so this restart
# is expected to report failure. What it must never do is report success on the
# strength of a probe that belonged to a process it did not start.
[ "$RESTART_RC" -ne 0 ]
check "$?" "the restart reports the failure it actually saw"

echo "--- probe opens and passes after the restart ---"
echo "$AFTER" | grep -E "Waiting for health check of 'web'|Health check passed for 'web'|Starting service: web" | tail -12

sysg stop --supervisor >/dev/null 2>&1
finish
