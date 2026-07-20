#!/usr/bin/env bash
# USE CASE: purge is REFUSED while a live supervisor is managing processes.
#
# WHAT THIS TESTS
#   The core purge guard. Wiping state out from under a running supervisor
#   strands its processes and corrupts its view of the world. A bare `sysg purge`
#   against a serving supervisor with live units must refuse with SG0401, delete
#   NOTHING, and leave every service running.
#
# HARD INVARIANTS
#   - `sysg purge` exits non-zero and prints SG0401,
#   - the state dir still exists, both services still running,
#   - the supervisor still answers.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
STATE_DIR="$HOME/.local/share/systemg"

section "boot the stack"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3
[ "$(pgrep -c -x sleep || echo 0)" = "2" ]
check "$?" "2 services running before purge"

section "bare purge is refused with SG0401"
sysg purge >/tmp/p.out 2>/tmp/p.err
RC=$?
cat /tmp/p.out /tmp/p.err
[ "$RC" != "0" ]
check "$?" "purge exits non-zero (refused)"
grep -q "SG0401" /tmp/p.err
check "$?" "refusal names SG0401"

section "purge deleted NOTHING; the stack is intact"
[ -d "$STATE_DIR" ]
check "$?" "state dir still present"
[ "$(pgrep -c -x sleep || echo 0)" = "2" ]
check "$?" "both services still running after the refused purge"
S="$(sysg status --config "$CONFIG" --format json 2>/tmp/st.err)"
grep -qi "No running supervisor" /tmp/st.err && DEAD=1 || DEAD=0
[ "$DEAD" = "0" ]
check "$?" "supervisor still answering after the refused purge"
[ "$(unit_count "$S")" = "2" ]
check "$?" "status still lists 2 units"

sysg stop --supervisor >/dev/null 2>&1
finish
