#!/usr/bin/env bash
# USE CASE: a restart that bounces a unit into a command that dies is a HARD fail.
#
# WHAT THIS TESTS
#   `restart -c <file>` reconciles the live set to a new manifest and must WAIT
#   for each bounced unit to actually come up. If worker is bounced to a command
#   that immediately exits, the restart is a hard failure -- SG0302 (reconcile
#   incomplete) on the terminal and a non-zero exit -- NOT a false "restarted"
#   success that leaves a dead unit behind. restart_policy is never so the dead
#   command can't retry-loop and mask the failure.
#
# EXPECTED OUTCOME
#   - Boot demo (worker) running.
#   - Overwrite the config so worker's command exits immediately.
#   - `sysg restart -c <file>` exits NON-ZERO with SG0302 on the terminal.
#   Expected RED until the reconcile+came-up gate lands.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot the valid config"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3
S1="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
[ "$(unit_field "$S1" worker state)" = "running" ]
check "$?" "worker running before restart"

section "overwrite so worker's command exits immediately"
cat > "$CONFIG" <<'EOF'
version: "2"
projects:
  demo:
    name: Demo
    services:
      worker:
        command: "sh -c 'exit 1'"
        restart_policy: "never"
        deployment:
          strategy: immediate
EOF
echo "config now bounces worker into a command that dies"

section "restart is a HARD failure with SG0302"
sysg restart --config "$CONFIG" 2>/tmp/fail.txt
RC=$?
cat /tmp/fail.txt
[ "$RC" != "0" ]
check "$?" "restart into a dying command exits non-zero"
stderr_has_code SG0302 /tmp/fail.txt
check "$?" "stderr names SG0302 (reconcile incomplete)"

sysg stop --supervisor >/dev/null 2>&1
finish
