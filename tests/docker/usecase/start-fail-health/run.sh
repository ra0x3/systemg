#!/usr/bin/env bash
# USE CASE: start a project whose service never passes its health check.
#
# WHAT THIS TESTS
#   `sysg start -c stack.yaml --daemonize` on a project `demo` with one service
#   `unhealthy`. The process starts and stays up (`sleep 3000`), but its
#   configured `deployment.health_check` (`sh -c 'exit 1'`) never passes. start
#   MUST report the typed error SG0104 on the terminal (stderr), not a generic
#   SG0001 and not a false success just because the process is alive.
#
# EXPECTED OUTCOME
#   - start exits non-zero (the unit never became healthy).
#   - captured stderr names SG0104.
#   EXPECTED RED until the start rebuild's came-up gate + boot-progress
#   streaming lands. This red is intentional and correct.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "start the never-healthy project"
sysg start --config "$CONFIG" --daemonize 2>/tmp/err.txt
RC=$?
echo "start rc: $RC"
cat /tmp/err.txt

[ "$RC" != "0" ]
check "$?" "start exits non-zero when the unit never becomes healthy"
stderr_has_code SG0104 /tmp/err.txt
check "$?" "stderr surfaces the typed code SG0104"

sysg stop --config "$CONFIG" >/dev/null 2>&1 || true
finish
