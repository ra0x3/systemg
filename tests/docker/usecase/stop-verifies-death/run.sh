#!/usr/bin/env bash
# USE CASE: stop confirms the processes actually died.
#
# WHAT THIS TESTS (real dogfooding bug)
#   `sysg stop -p <project>` reported "stopped" and status showed every unit as
#   Stopped while FOUR service processes were still running and still holding
#   their ports. The stop wrote the `stopped` record unconditionally: the kill
#   could no-op entirely (a stale recorded pgid signals a group that no longer
#   belongs to the service, and ESRCH is swallowed as "already dead") and the
#   state was recorded as success regardless.
#
#   A supervisor that reports a lie about what is running is worse than one that
#   reports an error — the user moves on believing the port is free.
#
#   `ignorer` traps SIGTERM so a polite stop cannot kill it; only the escalation
#   to SIGKILL can. That makes the verification observable: if stop returns
#   success, the process must genuinely be gone, not merely signalled.
#
# EXPECTED OUTCOME
#   - both services are running to begin with;
#   - after `stop -p`, NO service process survives — including the one that
#     ignores SIGTERM;
#   - status agrees with reality (no unit claims to be running);
#   - `ps` and sysg tell the same story.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "start the project"
sysg start --config "$CONFIG" --daemonize >/dev/null 2>&1
check "$?" "project started"
sleep 5

S1="$(sysg status --format json 2>/dev/null)"
IGN_PID="$(unit_field "$S1" ignorer pid)"
NRM_PID="$(unit_field "$S1" normal pid)"
echo "ignorer pid=$IGN_PID  normal pid=$NRM_PID"

pid_alive "$IGN_PID"
check "$?" "the SIGTERM-ignoring service is running"
pid_alive "$NRM_PID"
check "$?" "the normal service is running"

section "stop the project"
sysg stop -p stubborn >/tmp/stop.out 2>&1
STOP_RC=$?
echo "stop rc: $STOP_RC"
cat /tmp/stop.out
sleep 3

section "what sysg CLAIMS must match what the OS shows"
! pid_alive "$NRM_PID"
check "$?" "the normal service is genuinely gone (pid $NRM_PID)"

! pid_alive "$IGN_PID"
check "$?" "the SIGTERM-IGNORING service is genuinely gone (pid $IGN_PID)"

# The whole point: a success report must not coexist with survivors.
if [ "$STOP_RC" = "0" ]; then
  ! pid_alive "$IGN_PID" && ! pid_alive "$NRM_PID"
  check "$?" "stop reported success AND every process is actually dead"
else
  echo "stop reported failure; that is acceptable only if something survived"
  pid_alive "$IGN_PID" || pid_alive "$NRM_PID"
  check "$?" "a non-zero stop is explained by a real survivor"
fi

section "status does not claim a stopped unit is running"
S2="$(sysg status --format json 2>/dev/null)"
[ "$(unit_field "$S2" ignorer state)" != "running" ]
check "$?" "ignorer is not reported as running"
[ "$(unit_field "$S2" normal state)" != "running" ]
check "$?" "normal is not reported as running"

sysg stop --supervisor >/dev/null 2>&1
finish
