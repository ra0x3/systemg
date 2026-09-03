#!/usr/bin/env bash
# USE CASE: restart -c starts an ADDED service, and --delta says who else moves.
#
# WHAT THIS TESTS
#   Project `demo` starts with one service, web. We rewrite the config to ADD a
#   second service, worker, and restart.
#
#   `restart -c` means restart: worker starts AND web is bounced, because a
#   manifest diff cannot see a rebuilt binary at an unchanged path and a scope
#   read off that diff would leave the old process running behind exit 0.
#   `--delta` is how a caller asks for the additive reconcile instead: the added
#   unit starts and everything already running keeps its pid.
#
# EXPECTED OUTCOME
#   - After restart -c: worker is running with a live pid, and web has a NEW pid.
#   - After a --delta restart that adds sidecar: sidecar runs and web/worker pids
#     are UNCHANGED.
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
[ "$WEB_2" != "$WEB_1" ] && pid_alive "$WEB_2"
check "$?" "web pid CHANGED (restart means restart)"

section "add sidecar and reconcile with --delta"
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
      sidecar:
        command: "sleep 3000"
        restart_policy: "always"
EOF
sysg restart --config "$CONFIG" --delta
check "$?" "restart --delta exits 0"
sleep 3
S3="$(sysg status --format json 2>/dev/null)"
WEB_3="$(unit_field "$S3" web pid demo)"
WORKER_3="$(unit_field "$S3" worker pid demo)"
SIDECAR_3="$(unit_field "$S3" sidecar pid demo)"
echo "delta  -> web:$WEB_3 worker:$WORKER_3 sidecar:$SIDECAR_3"

[ "$(unit_field "$S3" sidecar state demo)" = "running" ] && pid_alive "$SIDECAR_3"
check "$?" "sidecar started under --delta"
[ "$WEB_3" = "$WEB_2" ] && [ "$WORKER_3" = "$WORKER_2" ]
check "$?" "web and worker pids UNCHANGED (--delta adopts what it did not change)"

sysg stop --supervisor >/dev/null 2>&1
finish
