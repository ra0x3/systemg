#!/usr/bin/env bash
# USE CASE: a unit that takes longer to serve than the periodic health sweep's
# interval is restarted by an operator.
#
# WHAT THIS TESTS
#   The supervisor re-probes every URL health check on a 30s timer and stops any
#   unit that fails. That sweep knew nothing about a unit still inside its
#   startup readiness gate, so a unit whose manifest grants it a 120s readiness
#   budget got killed 30s in, while the start that launched it was still
#   waiting. The start then found a different process holding the unit and gave
#   up with "a newer generation replaced the process this start launched", which
#   reached the operator as SG0001. Reproduced here with a unit that serves
#   nothing for its first 45s, exactly like a dev server that compiles first.
#
# EXPECTED OUTCOME
#   - Boot completes: the sweep does not kill the unit during its first start.
#   - `sysg restart -s web` exits 0 and bounces the process, spanning at least
#     one sweep tick without being condemned.
#   - The supervisor log never says the sweep stopped web.
set -u
. /usecase/lib.sh

section "boot: readiness takes 45s, longer than the 30s sweep interval"
sysg start --config /usecase/stack.yaml --daemonize
check "$?" "start exits 0 (the sweep did not kill the unit mid-boot)"

BOOT_PID="$(unit_field "$(sysg status --config /usecase/stack.yaml --format json 2>/dev/null)" web pid demo)"
echo "pid after boot: ${BOOT_PID}"
[ -n "${BOOT_PID}" ] && [ "${BOOT_PID}" != "absent" ] && [ "${BOOT_PID}" != "noparse" ] && [ "${BOOT_PID}" != "null" ]
check "$?" "web is running after boot"

section "restart across a sweep tick"
sysg restart -s web >/tmp/restart.log 2>&1
RESTART_RC=$?
echo "restart exit code: ${RESTART_RC}"
cat /tmp/restart.log
[ "${RESTART_RC}" -eq 0 ]
check "$?" "restart exits 0"

check_fails "restart did not report a superseded start" grep -q "SG0305" /tmp/restart.log
check_fails "restart did not report SG0001" grep -q "SG0001" /tmp/restart.log

NEW_PID="$(unit_field "$(sysg status --config /usecase/stack.yaml --format json 2>/dev/null)" web pid demo)"
echo "pid after restart: ${NEW_PID}"
[ -n "${NEW_PID}" ] && [ "${NEW_PID}" != "absent" ] && [ "${NEW_PID}" != "noparse" ] && [ "${NEW_PID}" != "${BOOT_PID}" ]
check "$?" "the restart actually bounced the process"

section "the sweep's own verdict"
LOG="$(sysg logs --supervisor 2>/dev/null)"
SWEPT="$(echo "${LOG}" | grep -c "Service 'web' is running but failed its health check")"
echo "sweep stops of web: ${SWEPT}"
[ "${SWEPT}" -eq 0 ]
check "$?" "the sweep never condemned a unit that was still starting"

PASSED="$(echo "${LOG}" | grep -c "Health check passed for 'web'")"
echo "readiness passes: ${PASSED}"
[ "${PASSED}" -ge 2 ]
check "$?" "both the boot and the restart reached readiness on their own"

echo "--- lifecycle lines for web ---"
echo "${LOG}" | grep -E "Starting service: web|Waiting for health check of 'web'|Health check passed for 'web'|failed its health check" | tail -12

sysg stop --supervisor >/dev/null 2>&1
finish
