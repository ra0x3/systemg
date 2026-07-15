#!/usr/bin/env bash
# USE CASE: stopping with no supervisor running fails cleanly, not violently.
#
# WHAT THIS TESTS
#   Stopping when NO supervisor is running gives a clean, clear message and does
#   not crash or leave junk. We do NOT start anything -- there is no daemon to
#   talk to. The command must degrade gracefully, never panic.
#
# EXPECTED OUTCOME
#   - `sysg stop --service web` produces no Rust panic.
#   - It produces SOME message (non-empty stderr) or a clean exit.
#
#   This case documents desired behavior; it may be RED until the stop rebuild.
set -u
. /usecase/lib.sh

section "stop with no supervisor running"
sysg stop --service web --config /usecase/stack.yaml 2>/tmp/n.txt
RC=$?
echo "stop rc: $RC"
cat /tmp/n.txt

if grep -qi "panic" /tmp/n.txt; then
  check 1 "no panic"
else
  check 0 "stop with no supervisor does not panic"
fi

[ -s /tmp/n.txt ] || [ "$RC" = "0" ]
check "$?" "stop with no supervisor produced a clean result"

finish
