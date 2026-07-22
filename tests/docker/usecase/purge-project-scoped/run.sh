#!/usr/bin/env bash
# USE CASE: `purge -p <project>` wipes ONE project's state, not the others.
#
# WHAT THIS TESTS
#   Scoped purge. A two-project config (alpha, beta) leaves two project state
#   dirs. After the supervisor is stopped, `sysg purge -p alpha` must remove only
#   alpha's dir and leave beta's intact. A `-p` naming a project with no state
#   must refuse with SG0403 and delete nothing.
#
# HARD INVARIANTS
#   - both project dirs exist after boot+stop,
#   - `purge -p alpha` exits 0, removes projects/alpha, keeps projects/beta,
#   - `purge -p ghost` exits non-zero with SG0403 and touches nothing.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
PROJ_DIR="$HOME/.local/share/systemg/projects"

section "boot the two-project stack, then stop the supervisor"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3
sysg stop --supervisor >/tmp/stop.out 2>/tmp/stop.err
RC=$?
cat /tmp/stop.out /tmp/stop.err
[ "$RC" = "0" ]
check "$?" "stop --supervisor exits 0"
[ -d "$PROJ_DIR/alpha" ] && [ -d "$PROJ_DIR/beta" ]
check "$?" "both project state dirs present"

section "purge -p alpha removes only alpha"
sysg purge -p alpha >/tmp/p.out 2>/tmp/p.err
RC=$?
cat /tmp/p.out /tmp/p.err
[ "$RC" = "0" ]
check "$?" "purge -p alpha exits 0"
[ ! -d "$PROJ_DIR/alpha" ]
check "$?" "projects/alpha removed"
[ -d "$PROJ_DIR/beta" ]
check "$?" "projects/beta untouched"

section "purge -p ghost (no state) refuses with SG0403"
sysg purge -p ghost >/tmp/g.out 2>/tmp/g.err
RC=$?
cat /tmp/g.out /tmp/g.err
[ "$RC" != "0" ]
check "$?" "purge -p ghost exits non-zero"
grep -q "SG0403" /tmp/g.err
check "$?" "names SG0403 (no state for that project)"
[ -d "$PROJ_DIR/beta" ]
check "$?" "beta still present after the failed ghost purge"

finish
