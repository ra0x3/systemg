#!/usr/bin/env bash
# USE CASE: a whole-config restart with an INVALID new manifest touches nothing.
#
# WHAT THIS TESTS
#   The defensive core of restart: `restart -c <file>` must VALIDATE the entire
#   new manifest BEFORE tearing anything down. If the new manifest is invalid
#   (bad YAML / dependency cycle / missing command), the restart is refused
#   whole with SG0301 and the running services are left EXACTLY as they were.
#   No half-applied migration, no teardown-then-fail. This is the "supervisor
#   got fucked on restart" class — a bad config must never reach the live set.
#
# EXPECTED OUTCOME
#   - Boot demo (web, api) running; record their pids.
#   - Overwrite the config with an INVALID manifest (a dependency cycle).
#   - `sysg restart -c <file>` exits NON-ZERO with SG0301 on the terminal.
#   - web and api are STILL running on their ORIGINAL pids (nothing touched).
#   Expected RED until the validate-before-teardown reconcile lands.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot the valid config"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3
S1="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
WEB1="$(unit_field "$S1" web pid)"
API1="$(unit_field "$S1" api pid)"
echo "before -> web:$WEB1 api:$API1"
[ -n "$WEB1" ] && [ -n "$API1" ] && pid_alive "$WEB1" && pid_alive "$API1"
check "$?" "web and api running before restart"

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

section "restart is refused whole with SG0301; nothing touched"
sysg restart --config "$CONFIG" 2>/tmp/rej.txt
RC=$?
cat /tmp/rej.txt
[ "$RC" != "0" ]
check "$?" "restart with invalid manifest exits non-zero"
stderr_has_code SG0301 /tmp/rej.txt
check "$?" "stderr names SG0301 (manifest rejected)"

sleep 1
pid_alive "$WEB1"
check "$?" "web STILL alive on its original pid (untouched)"
pid_alive "$API1"
check "$?" "api STILL alive on its original pid (untouched)"

sysg stop --supervisor >/dev/null 2>&1
finish
