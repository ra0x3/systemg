#!/usr/bin/env bash
# USE CASE: a reconcile that REMOVES a service actually KILLS its process — not
# just drops it from status.
#
# WHAT THIS TESTS
#   The adversary removes a service from the manifest and restarts. A supervisor
#   that merely forgets the service (drops it from state.xml) while its process
#   keeps running has leaked an orphan — the exact "ps shows it, status doesn't"
#   divergence this whole rebuild exists to kill. Removal must verify death: the
#   old pid must be gone from the process table, and status must not list it.
#
# EXPECTED OUTCOME
#   - Boot demo (web, worker); record worker's pid.
#   - Remove `worker` from the manifest; restart -c.
#   - worker is ABSENT from status AND its old pid is DEAD in the process table.
#   - web is still running (untouched).
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot the config"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3
S1="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
WEB1="$(unit_field "$S1" web pid)"
WORKER1="$(unit_field "$S1" worker pid)"
echo "before -> web:$WEB1 worker:$WORKER1"
[ -n "$WEB1" ] && [ -n "$WORKER1" ] && pid_alive "$WEB1" && pid_alive "$WORKER1"
check "$?" "web and worker running before restart"

section "remove worker from the manifest, then restart -c"
cat > "$CONFIG" <<'EOF'
version: "2"
projects:
  demo:
    name: Demo
    services:
      web:
        command: "sleep 3000"
EOF
echo "config now has only web"

sysg restart --config "$CONFIG"
check "$?" "restart -c exits 0"
sleep 3

section "worker is GONE from status AND dead in the process table"
S2="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
WORKER2="$(unit_field "$S2" worker pid)"
echo "after  -> worker in status:'${WORKER2}'"
[ "$WORKER2" = "absent" ]
check "$?" "worker absent from status (removed from the managed set)"

if pid_alive "$WORKER1"; then
  check 1 "worker's old process is DEAD (no orphan left running)"
else
  check 0 "worker's old process is DEAD (no orphan left running)"
fi

WEB2="$(unit_field "$S2" web pid)"
[ -n "$WEB2" ] && pid_alive "$WEB2"
check "$?" "web still running (untouched by the removal)"

sysg stop --supervisor >/dev/null 2>&1
finish
