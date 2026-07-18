#!/usr/bin/env bash
# USE CASE: the supervisor stays WARM after its last project stops.
#
# WHAT THIS TESTS
#   The supervisor is impartial, warm-persistent infrastructure. Stopping its
#   ONLY project must NOT shut it down — it idles, ready for the next start.
#   It ends ONLY on an explicit `sysg stop --supervisor`.
#
# EXPECTED OUTCOME
#   - start one project (the only one); supervisor up.
#   - `stop -p demo` stops the project but the supervisor is STILL running.
#   - a fresh `start` re-registers into the SAME warm supervisor.
#   - `stop --supervisor` finally ends it.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "start the only project"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 2
pgrep -x sysg >/dev/null
check "$?" "supervisor process is running"

section "stop the ONLY project — supervisor must stay warm"
sysg stop -p demo
check "$?" "stop -p demo exits 0"
sleep 2
pgrep -x sysg >/dev/null
check "$?" "supervisor is STILL running after its last project stopped (warm)"

section "a fresh start re-registers into the same warm supervisor"
SUP_PID_BEFORE="$(pgrep -x sysg | head -1)"
sysg start --config "$CONFIG" --daemonize
check "$?" "second start exits 0"
sleep 2
SUP_PID_AFTER="$(pgrep -x sysg | head -1)"
echo "supervisor pid: before=$SUP_PID_BEFORE after=$SUP_PID_AFTER"
[ -n "$SUP_PID_AFTER" ] && [ "$SUP_PID_AFTER" = "$SUP_PID_BEFORE" ]
check "$?" "same warm supervisor hosted the re-start (pid unchanged)"

section "explicit stop --supervisor finally ends it"
sysg stop --supervisor
check "$?" "stop --supervisor exits 0"
# The supervisor unwinds its monitor threads over a couple seconds; poll for it.
GONE=0
for _ in $(seq 1 10); do
  sleep 1
  pgrep -x sysg >/dev/null || { GONE=1; break; }
done
[ "$GONE" = "1" ]
check "$?" "supervisor is gone only after an explicit stop --supervisor"

finish
