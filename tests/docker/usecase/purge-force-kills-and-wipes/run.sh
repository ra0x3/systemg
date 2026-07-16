#!/usr/bin/env bash
# USE CASE: `purge --force` overrides the guard — stops the supervisor, wipes all.
#
# WHAT THIS TESTS
#   The escape hatch. `--force` is the explicit "I accept the teardown" signal:
#   it must stop the live supervisor, kill its services, and remove the whole
#   state root. After it, no supervisor, no service processes, no state dir.
#
# HARD INVARIANTS
#   - `sysg purge --force` exits 0,
#   - no `sleep` service processes remain,
#   - the state dir is gone,
#   - a fresh `status` reports no running supervisor.
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

section "purge --force stops the supervisor and wipes everything"
sysg purge --force >/tmp/p.out 2>/tmp/p.err
RC=$?
cat /tmp/p.out /tmp/p.err
[ "$RC" = "0" ]
check "$?" "purge --force exits 0"
sleep 2

section "nothing survives the forced purge"
NOW="$(pgrep -x sleep | wc -l | tr -d ' ')"
echo "sleep procs after purge: $NOW (expected 0)"
[ "$NOW" = "0" ]
check "$?" "no service processes remain"
[ ! -d "$STATE_DIR" ]
check "$?" "state dir is gone"
sysg status --config "$CONFIG" >/tmp/st.txt 2>&1
grep -qi "SG0206\|No running supervisor\|OFFLINE" /tmp/st.txt
check "$?" "status reports no running supervisor after purge"

finish
