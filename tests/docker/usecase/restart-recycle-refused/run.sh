#!/usr/bin/env bash
# USE CASE: a version-drifted supervisor recycle is REFUSED when the replacement
# config is invalid — the old daemon is never torn down.
#
# WHAT THIS TESTS
#   When the resident supervisor runs a different version than the CLI, a full
#   `sysg restart` must RECYCLE it (stop the old daemon, start a fresh one). That
#   recycle validates the replacement config BEFORE stopping anything: a bad
#   config must never cost you the running stack. This is the aerospace rule —
#   never trade a working supervisor for an unvalidated one. The failure is
#   typed SG0303, and the old supervisor keeps answering.
#
# HARNESS
#   The image ships two binaries: `sysg-old` built at version 0.0.1 (the resident
#   daemon) and `sysg` at the real version (the CLI). Starting the daemon with
#   sysg-old and driving the restart with sysg forces genuine version drift, so
#   the CLI takes the recycle path rather than a same-version reconcile.
#
# EXPECTED OUTCOME
#   - `sysg-old` boots demo (web, api); record their pids.
#   - Overwrite the config with an INVALID manifest (a dependency cycle).
#   - `sysg restart -c <file>` (real CLI) detects drift, tries to recycle, and is
#     REFUSED: exits non-zero with SG0303 on the terminal.
#   - The old supervisor is STILL answering and web/api are STILL on their
#     original pids (nothing torn down).
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot the valid config with the OLD (v0.0.1) supervisor"
sysg-old start --config "$CONFIG" --daemonize
check "$?" "old supervisor start exits 0"
sleep 3
S1="$(sysg-old status --config "$CONFIG" --format json 2>/dev/null)"
WEB1="$(unit_field "$S1" web pid)"
API1="$(unit_field "$S1" api pid)"
echo "before -> web:$WEB1 api:$API1"
[ -n "$WEB1" ] && [ -n "$API1" ] && pid_alive "$WEB1" && pid_alive "$API1"
check "$?" "web and api running before restart"

section "confirm the CLI sees version drift"
sysg --version 2>/dev/null | grep -qv "0.0.1"
check "$?" "CLI is NOT v0.0.1 (drift vs the resident daemon)"

section "overwrite with an INVALID manifest (dependency cycle)"
cat > "$CONFIG" <<'EOF'
version: "2"
projects:
  demo:
    name: Demo
    services:
      web:
        command: "sleep 3000"
        depends_on: [api]
      api:
        command: "sleep 3000"
        depends_on: [web]
EOF
echo "config now has a web<->api dependency cycle"

section "recycle is refused with SG0303; the old supervisor is untouched"
sysg restart --config "$CONFIG" 2>/tmp/rej.txt
RC=$?
cat /tmp/rej.txt
[ "$RC" != "0" ]
check "$?" "restart with invalid manifest exits non-zero"
stderr_has_code SG0303 /tmp/rej.txt
check "$?" "stderr names SG0303 (supervisor recycle refused)"

sleep 1
pid_alive "$WEB1"
check "$?" "web STILL alive on its original pid (old daemon untouched)"
pid_alive "$API1"
check "$?" "api STILL alive on its original pid (old daemon untouched)"

sysg-old status --config "$CONFIG" --format json >/tmp/sup.txt 2>&1
if grep -qi "No running supervisor" /tmp/sup.txt; then
  check 1 "old supervisor still answering status"
else
  check 0 "old supervisor still answering status"
fi

sysg-old stop --supervisor >/dev/null 2>&1
finish
