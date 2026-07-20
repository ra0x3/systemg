#!/usr/bin/env bash
# USE CASE: restart -c reconciles a REMOVED service out of a running project.
#
# WHAT THIS TESTS
#   Project `demo` starts with two services, web and worker. We then rewrite the
#   config to REMOVE worker (only web remains) and run `sysg restart --config`.
#   Reconcile must STOP the removed worker while leaving web running — dropping
#   a service from the config tears down exactly that service.
#
# EXPECTED OUTCOME
#   - After restart -c: worker's recorded pid is DEAD and status no longer shows
#     it running (reconcile removed it).
#   - web is STILL running and its pid is UNCHANGED (kept service left alone).
#
# NOTE: expected RED until the reconcile-on-restart behavior lands.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot the two-service project"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3
S1="$(sysg status --format json 2>/dev/null)"
WEB_1="$(unit_field "$S1" web pid demo)"
WORKER_PID="$(unit_field "$S1" worker pid demo)"
echo "before -> web:$WEB_1 worker:$WORKER_PID"
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
[ "$WEB_2" = "$WEB_1" ]
check "$?" "web pid UNCHANGED (kept service not disturbed)"

sysg stop --supervisor >/dev/null 2>&1
finish
