#!/usr/bin/env bash
# USE CASE: restart -c --delta reconciles a CHANGED service command.
#
# WHAT THIS TESTS
#   Project `demo` has one service, worker, whose command writes a marker file
#   (V1) then sleeps. We then rewrite the config to change worker's command to
#   write V2 and run `sysg restart --config --delta`. The delta must BOUNCE worker onto
#   the new command — the marker flips to V2 and worker gets a new pid, with no
#   stale V1 instance left behind.
#
# EXPECTED OUTCOME
#   - Before: marker.txt contains V1.
#   - After restart -c --delta: marker.txt contains V2 (the new command ran),
#     worker has a NEW pid, is running, and that pid is alive.

set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
MARKER=/usecase/marker.txt

section "boot the project on the V1 command"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3
S1="$(sysg status --format json 2>/dev/null)"
WORKER_1="$(unit_field "$S1" worker pid demo)"
echo "before -> worker:$WORKER_1 marker:$(cat "$MARKER" 2>/dev/null)"
grep -q "V1" "$MARKER" 2>/dev/null
check "$?" "marker contains V1 before restart"

section "change worker's command to V2 and restart -c"
cat > "$CONFIG" <<'EOF'
version: "2"
projects:
  demo:
    name: Demo
    services:
      worker:
        command: "sh -c 'echo V2 > /usecase/marker.txt && exec sleep 3000'"
        restart_policy: "always"
EOF
sysg restart --config "$CONFIG" --delta
check "$?" "restart -c --delta exits 0"
sleep 3
S2="$(sysg status --format json 2>/dev/null)"
WORKER_2="$(unit_field "$S2" worker pid demo)"
echo "after  -> worker:$WORKER_2 marker:$(cat "$MARKER" 2>/dev/null)"

grep -q "V2" "$MARKER" 2>/dev/null
check "$?" "marker contains V2 (new command ran)"
[ -n "$WORKER_2" ] && [ "$WORKER_2" != "$WORKER_1" ]
check "$?" "worker restarted (pid changed)"
[ "$(unit_field "$S2" worker state demo)" = "running" ]
check "$?" "worker is running after restart"
pid_alive "$WORKER_2"
check "$?" "worker's new pid is actually alive"

sysg stop --supervisor >/dev/null 2>&1
finish
