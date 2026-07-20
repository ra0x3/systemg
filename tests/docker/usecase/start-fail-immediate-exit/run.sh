#!/usr/bin/env bash
# USE CASE: start a project whose only service exits immediately.
#
# WHAT THIS TESTS
#   `sysg start -c stack.yaml --daemonize` on a project `demo` with one service
#   `crasher` whose command (`sh -c 'exit 1'`) exits before it can ever come up.
#   start MUST report the typed error SG0102 on the terminal (stderr), not hang,
#   not falsely succeed, and not collapse it into a generic SG0001.
#
# EXPECTED OUTCOME
#   - start exits non-zero (the requested unit could not come up).
#   - captured stderr names SG0102.
#   EXPECTED RED until the start rebuild's came-up gate + boot-progress
#   streaming lands. This red is intentional and correct.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "start the immediate-exit project"
sysg start --config "$CONFIG" --daemonize 2>/tmp/err.txt
RC=$?
echo "start rc: $RC"
cat /tmp/err.txt

[ "$RC" != "0" ]
check "$?" "start exits non-zero when the unit can't come up"
stderr_has_code SG0102 /tmp/err.txt
check "$?" "stderr surfaces the typed code SG0102"

sysg stop --config "$CONFIG" >/dev/null 2>&1 || true
finish
