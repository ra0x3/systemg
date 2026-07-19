#!/usr/bin/env bash
# USE CASE: health-check failures carry the RIGHT code, and a refused endpoint
# fails FAST (not a long hang).
#
# WHAT THIS TESTS
#   The prod pain: a service whose health check hit a taken port blocked a
#   foreground start for ~60s with no useful signal. Health checks now use a
#   per-probe `attempt_timeout` (max wait = retries x (attempt_timeout + interval),
#   derived), and the failure carries one of three codes by WHY it failed:
#     - SG0022 the check could not reach the service (refused / not executable),
#     - SG0023 a probe exceeded its attempt_timeout,
#     - SG0104 the check ran and reported unhealthy.
#   A refused endpoint returns immediately, so it fails FAST.
#
# HARD INVARIANTS
#   - refused URL -> SG0022, and it fails in a few seconds (not tens of seconds),
#   - health command that exits non-zero -> SG0104,
#   - health command that outruns attempt_timeout -> SG0023.
set -u
. /usecase/lib.sh

mkcfg() {
  cat > /usecase/s.yaml <<EOF
version: "2"
projects:
  dev:
    services:
      web:
        command: "sleep 3000"
        deployment:
          strategy: immediate
          health_check:
$1
EOF
}

section "refused URL -> SG0022, and FAST"
mkcfg '            url: "http://127.0.0.1:59999/health"
            attempt_timeout: "2s"
            retries: 3
            interval: "1s"'
T0="$(date +%s)"
sysg start --config /usecase/s.yaml --daemonize >/tmp/r.out 2>/tmp/r.err
T1="$(date +%s)"
cat /tmp/r.err | grep -v WARN | head -4
grep -q "SG0022" /tmp/r.err
check "$?" "refused health URL is SG0022 (unreachable)"
! grep -q "SG0001" /tmp/r.err
check "$?" "typed health failure is not wrapped in SG0001"
ELAPSED=$((T1 - T0))
echo "elapsed: ${ELAPSED}s (max wait = 3 x (2s+1s) = 9s; refused returns instantly so ~a few s)"
[ "$ELAPSED" -lt 15 ]
check "$?" "refused endpoint fails FAST (< 15s, not a long hang)"
sysg purge --force >/dev/null 2>&1

section "total readiness timeout survives fast failures during a slow start"
rm -f /tmp/health-ready
sh -c 'sleep 5; touch /tmp/health-ready' &
mkcfg '            command: "test -f /tmp/health-ready"
            timeout: "8s"
            attempt_timeout: "1s"
            retries: 2
            interval: "1s"'
T0="$(date +%s)"
sysg start --config /usecase/s.yaml --daemonize >/tmp/b.out 2>/tmp/b.err
BUDGET_RC=$?
T1="$(date +%s)"
cat /tmp/b.err | grep -v WARN | head -4
check "$BUDGET_RC" "service becomes healthy inside its total readiness budget"
ELAPSED=$((T1 - T0))
[ "$ELAPSED" -ge 4 ] && [ "$ELAPSED" -lt 12 ]
check "$?" "fast failures keep probing past the retry floor"
sysg purge --force >/dev/null 2>&1

section "health command that exits non-zero -> SG0104"
mkcfg '            command: "false"
            attempt_timeout: "2s"
            retries: 2
            interval: "1s"'
sysg start --config /usecase/s.yaml --daemonize >/tmp/u.out 2>/tmp/u.err
cat /tmp/u.err | grep -v WARN | head -3
grep -q "SG0104" /tmp/u.err
check "$?" "command that exits non-zero is SG0104 (ran, unhealthy)"
sysg purge --force >/dev/null 2>&1

section "health command that outruns attempt_timeout -> SG0023"
mkcfg '            command: "sleep 30"
            attempt_timeout: "1s"
            retries: 2
            interval: "1s"'
sysg start --config /usecase/s.yaml --daemonize >/tmp/t.out 2>/tmp/t.err
cat /tmp/t.err | grep -v WARN | head -3
grep -q "SG0023" /tmp/t.err
check "$?" "command exceeding attempt_timeout is SG0023 (timeout)"
sysg purge --force >/dev/null 2>&1

finish
