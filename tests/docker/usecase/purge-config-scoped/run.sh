#!/usr/bin/env bash
# USE CASE: `purge -c <config>` wipes every project the config declares.
#
# WHAT THIS TESTS
#   Config-scoped purge. The config declares alpha + beta. After the supervisor
#   is stopped, `sysg purge -c <config>` must remove BOTH project dirs (every
#   project the config names) plus the supervisor runtime files, while leaving an
#   UNRELATED project's state (registered separately) untouched.
#
# HARD INVARIANTS
#   - alpha + beta dirs exist after boot+stop,
#   - an unrelated project 'gamma' also has state,
#   - `purge -c <config>` exits 0, removes alpha AND beta, keeps gamma.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
PROJ_DIR="$HOME/.local/share/systemg/projects"
GAMMA=/usecase/gamma.yaml

section "register an unrelated project 'gamma' too"
cat >"$GAMMA" <<'EOF'
version: "2"
projects:
  gamma:
    name: Gamma
    services:
      worker:
        command: "sleep 3000"
EOF
sysg start --config "$CONFIG" --daemonize
check "$?" "start alpha+beta exits 0"
sleep 2
sysg start --config "$GAMMA" --daemonize
check "$?" "register gamma exits 0"
sleep 2
sysg stop --supervisor >/dev/null 2>&1
sleep 2
[ -d "$PROJ_DIR/alpha" ] && [ -d "$PROJ_DIR/beta" ] && [ -d "$PROJ_DIR/gamma" ]
check "$?" "alpha, beta, gamma state dirs all present"

section "purge -c <config> removes alpha+beta, keeps gamma"
sysg purge -c "$CONFIG" >/tmp/p.out 2>/tmp/p.err
RC=$?
cat /tmp/p.out /tmp/p.err
[ "$RC" = "0" ]
check "$?" "purge -c exits 0"
[ ! -d "$PROJ_DIR/alpha" ]
check "$?" "projects/alpha removed"
[ ! -d "$PROJ_DIR/beta" ]
check "$?" "projects/beta removed"
[ -d "$PROJ_DIR/gamma" ]
check "$?" "projects/gamma (not in the config) untouched"

finish
