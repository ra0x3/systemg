#!/usr/bin/env bash
# USE CASE: restart -c reconciles an ADDED service into a running project.
#
# WHAT THIS TESTS
#   Project `demo` starts with one service, web. We then rewrite the config to
#   ADD a second service, worker, and run `sysg restart --config`. Reconcile
#   must START the newly-added worker WITHOUT disturbing the already-running
#   web — adding a service is additive, not a full teardown.
#
# EXPECTED OUTCOME
#   - After restart -c: worker is running with a live pid (reconcile added it).
#   - web is STILL running and its pid is UNCHANGED (existing service left alone).
#
# NOTE: expected RED until the reconcile-on-restart behavior lands.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot the single-service project"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3
S1="$(sysg status --format json 2>/dev/null)"
WEB_1="$(unit_field "$S1" web pid demo)"
echo "before -> web:$WEB_1"

section "add worker to the config and restart -c"
cat > "$CONFIG" <<'EOF'
version: "2"
projects:
  demo:
    name: Demo
    services:
      web:
        command: "sleep 3000"
        restart_policy: "always"
      worker:
        command: "sleep 3000"
        restart_policy: "always"
EOF
sysg restart --config "$CONFIG"
check "$?" "restart -c exits 0"
sleep 3
S2="$(sysg status --format json 2>/dev/null)"
WEB_2="$(unit_field "$S2" web pid demo)"
WORKER_2="$(unit_field "$S2" worker pid demo)"
echo "after  -> web:$WEB_2 worker:$WORKER_2"

[ "$(unit_field "$S2" worker state demo)" = "running" ]
check "$?" "worker is running (reconcile added it)"
pid_alive "$WORKER_2"
check "$?" "worker's pid is actually alive"

[ "$(unit_field "$S2" web state demo)" = "running" ]
check "$?" "web is still running"
[ "$WEB_2" = "$WEB_1" ]
check "$?" "web pid UNCHANGED (existing service not disturbed)"

sysg stop --supervisor >/dev/null 2>&1
finish
