#!/usr/bin/env bash
# USE CASE: restart -c stops a REMOVED service, and a removal alone is not a
# restart.
#
# WHAT THIS TESTS
#   Project `demo` starts with web, worker and sidecar. Removing worker from the
#   config and restarting must tear down exactly worker — and, because `restart`
#   means restart, bounce the units that stayed.
#
#   The second half is the arbitration production bug: a `--delta` run that only
#   REMOVES a unit replaces no running process, yet the old guard treated
#   "stopped a removed unit" as work and returned exit 0 while every long-running
#   service kept its old binary. Removing a unit must not buy that exemption:
#   the run raises SG0304 and the surviving unit's pid proves nothing bounced.
#
# EXPECTED OUTCOME
#   - After restart -c: worker's pid is DEAD, status no longer runs it, and web's
#     pid CHANGED.
#   - After restart --delta that only removes sidecar: sidecar is stopped (the
#     removal is applied, then the verdict fails), the command exits NON-ZERO
#     with SG0304 on stderr, and web's pid is UNCHANGED.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot the three-service project"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3
S1="$(sysg status --format json 2>/dev/null)"
WEB_1="$(unit_field "$S1" web pid demo)"
WORKER_PID="$(unit_field "$S1" worker pid demo)"
SIDECAR_PID="$(unit_field "$S1" sidecar pid demo)"
echo "before -> web:$WEB_1 worker:$WORKER_PID sidecar:$SIDECAR_PID"
pid_alive "$WEB_1"
check "$?" "web alive before restart"
pid_alive "$WORKER_PID"
check "$?" "worker alive before restart"

section "remove worker from the config and restart -c"
cat > "$CONFIG" <<'EOF'
version: "2"
projects:
  demo:
    name: Demo
    services:
      web:
        command: "sleep 3000"
        restart_policy: "always"
      sidecar:
        command: "sleep 3000"
        restart_policy: "always"
EOF
sysg restart --config "$CONFIG"
check "$?" "restart -c exits 0"
sleep 3
S2="$(sysg status --format json 2>/dev/null)"
WEB_2="$(unit_field "$S2" web pid demo)"
echo "after  -> web:$WEB_2 worker-state:$(unit_field "$S2" worker state demo)"

if pid_alive "$WORKER_PID"; then
  check 1 "worker still alive after removal"
else
  check 0 "worker stopped after removal"
fi
[ "$(unit_field "$S2" worker state demo)" != "running" ]
check "$?" "worker not running in status (removed)"

[ "$(unit_field "$S2" web state demo)" = "running" ]
check "$?" "web is still running"
[ "$WEB_2" != "$WEB_1" ]
check "$?" "web pid CHANGED (restart means restart)"

section "remove sidecar under --delta: a removal is not a bounce"
cat > "$CONFIG" <<'EOF'
version: "2"
projects:
  demo:
    name: Demo
    services:
      web:
        command: "sleep 3000"
        restart_policy: "always"
EOF
sysg restart --config "$CONFIG" --delta >/tmp/delta.out 2>/tmp/delta.err
RC=$?
cat /tmp/delta.out /tmp/delta.err | tail -20
[ "$RC" != "0" ]
check "$?" "restart --delta that bounced nothing exits non-zero"
stderr_has_code SG0304 /tmp/delta.err
check "$?" "stderr names SG0304 (restart touched nothing)"
sleep 2
if pid_alive "$SIDECAR_PID"; then
  check 1 "sidecar still alive after removal"
else
  check 0 "sidecar stopped: the removal was applied before the verdict"
fi
S3="$(sysg status --format json 2>/dev/null)"
[ "$(unit_field "$S3" sidecar state demo)" != "running" ]
check "$?" "sidecar not running in status (removed)"
WEB_3="$(unit_field "$S3" web pid demo)"
[ "$WEB_3" = "$WEB_2" ]
check "$?" "web pid UNCHANGED, which is exactly what SG0304 reported"

sysg stop --supervisor >/dev/null 2>&1
finish
