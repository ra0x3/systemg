#!/usr/bin/env bash
# USE CASE (UNHAPPY): a service that can't own its declared port is a FAILED start.
#
# WHAT THIS TESTS (sysg ethos: no fallbacks)
#   A web dev server (yarn/vite/http.server) told to serve on a port will happily
#   DRIFT to the next free port if its own is taken — a silent fallback that
#   masks failure. sysg's leverage is the health check: if the health_check
#   targets the DECLARED port and the app moved, the check can't reach it, so the
#   unit must be reported FAILED — never "healthy on a drifted port".
#
#   Here port 39187 is pre-occupied by a blocker; `web`'s health_check targets
#   39187 (the declared port), but the process actually serves on 28641. sysg must
#   mark web unhealthy/failed (SG0022 unreachable — nothing sysg expects is on
#   39187), NOT healthy.
#
# HARD INVARIANTS
#   - the daemonized start does not report web healthy,
#   - web's health failure carries SG0022 (declared port unreachable),
#   - status shows web as failing/warn, not healthy.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "web drifts off its declared port; nothing valid answers on it"
# The app serves on 28641 (as if it drifted there because its real port was
# taken), while its health_check targets the DECLARED 39187 where nothing sysg
# expects is listening. sysg's leverage is the health check: it probes the
# declared port, gets no answer, and must FAIL the unit — never call it healthy
# just because the process is alive somewhere.
sysg start --config "$CONFIG" --daemonize >/tmp/start.out 2>/tmp/start.err
RC=$?
cat /tmp/start.err | grep -v WARN | head -8
[ "$RC" != "0" ]
check "$?" "start reports FAILURE (web never owned its declared port)"

section "sysg does NOT report web healthy on the drifted port"
sleep 2
S="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
HEALTH="$(unit_field "$S" web health dev)"
STATE="$(unit_field "$S" web state dev)"
echo "web -> state:$STATE health:$HEALTH"
[ "$HEALTH" != "healthy" ]
check "$?" "web is NOT reported healthy (it never owned its declared port)"

section "the failure names SG0022 (declared port unreachable)"
# the boot health failure is logged by the supervisor; check both start stderr and the log
if grep -q "SG0022" /tmp/start.err || grep -q "SG0022" "$HOME/.local/share/systemg/logs/supervisor.log" 2>/dev/null; then
  check 0 "health failure is SG0022 (could not reach the declared port)"
else
  echo "--- supervisor.log health lines ---"; grep -iE "health|SG0" "$HOME/.local/share/systemg/logs/supervisor.log" 2>/dev/null | grep -v capability | tail -4
  check 1 "health failure is SG0022 (could not reach the declared port)"
fi

sysg stop --supervisor >/dev/null 2>&1
sysg purge --force >/dev/null 2>&1
finish
