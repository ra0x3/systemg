#!/usr/bin/env bash
# USE CASE: start a project whose service has a failing pre_start command.
#
# WHAT THIS TESTS
#   `sysg start -c stack.yaml --daemonize` on a project `demo` with one service
#   `needs_setup` whose `deployment.pre_start` (`sh -c 'exit 3'`) fails. The
#   service MUST NOT start, and start MUST report the typed error SG0103 on the
#   terminal (stderr), not a generic SG0001.
#
# EXPECTED OUTCOME
#   - start exits non-zero (pre_start failed, so the unit never comes up).
#   - captured stderr names SG0103.
#   EXPECTED RED until the start rebuild's came-up gate + boot-progress
#   streaming lands. This red is intentional and correct.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "start the failing-pre_start project"
sysg start --config "$CONFIG" --daemonize 2>/tmp/err.txt
RC=$?
echo "start rc: $RC"
cat /tmp/err.txt

[ "$RC" != "0" ]
check "$?" "start exits non-zero when pre_start fails"
stderr_has_code SG0103 /tmp/err.txt
check "$?" "stderr surfaces the typed code SG0103"

sysg stop --config "$CONFIG" >/dev/null 2>&1 || true
finish
