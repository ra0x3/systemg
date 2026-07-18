#!/usr/bin/env bash
# USE CASE: purge with NO live supervisor cleanly wipes leftover state.
#
# WHAT THIS TESTS
#   The common case: the supervisor is already stopped, but state files linger.
#   A bare `sysg purge` must not be blocked by the guard (nothing is being
#   managed), and must remove the whole state root.
#
# HARD INVARIANTS
#   - after stopping the supervisor, state files still exist,
#   - `sysg purge` exits 0 (no SG0401 — nothing is managed),
#   - the state dir is gone afterward.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
STATE_DIR="$HOME/.local/share/systemg"

section "boot then stop the supervisor, leaving state behind"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 2
sysg stop --supervisor >/dev/null 2>&1
sleep 2
[ -d "$STATE_DIR/projects" ]
check "$?" "project state persists after supervisor stop"

section "purge cleanly wipes with no supervisor running"
sysg purge >/tmp/p.out 2>/tmp/p.err
RC=$?
cat /tmp/p.out /tmp/p.err
[ "$RC" = "0" ]
check "$?" "purge exits 0 (not refused — nothing managed)"
! grep -q "SG0401" /tmp/p.err
check "$?" "no SG0401 refusal when no supervisor is managing"

section "state is gone"
[ ! -d "$STATE_DIR" ]
check "$?" "state dir removed"

finish
