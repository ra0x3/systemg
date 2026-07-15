#!/usr/bin/env bash
# USE CASE: restart -p <project> is isolated to that project.
#
# WHAT THIS TESTS
#   Two projects, alpha (a1,a2) and beta (b1). `sysg restart -p alpha` must
#   bounce alpha's services (new pids) and leave beta COMPLETELY untouched
#   (same pid, still alive). This is the isolation guarantee — the sibling
#   teardown bug (restart -p killing other projects) must stay dead.
#
# EXPECTED OUTCOME
#   - After restart -p alpha: a1 and a2 have NEW pids and are running.
#   - b1's pid is UNCHANGED and its process is still alive (beta untouched).
#   - status shows all three running under their own projects.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot both projects"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3
S1="$(sysg status --format json 2>/dev/null)"
A1_1="$(unit_field "$S1" a1 pid alpha)"
A2_1="$(unit_field "$S1" a2 pid alpha)"
B1_1="$(unit_field "$S1" b1 pid beta)"
echo "before -> a1:$A1_1 a2:$A2_1 b1:$B1_1"
[ "$(unit_field "$S1" a1 state alpha)" = "running" ] && \
[ "$(unit_field "$S1" a2 state alpha)" = "running" ] && \
[ "$(unit_field "$S1" b1 state beta)" = "running" ]
check "$?" "all three running before restart"

section "restart -p alpha bounces alpha, leaves beta untouched"
sysg restart --project alpha
check "$?" "restart -p alpha exits 0"
sleep 3
S2="$(sysg status --format json 2>/dev/null)"
A1_2="$(unit_field "$S2" a1 pid alpha)"
A2_2="$(unit_field "$S2" a2 pid alpha)"
B1_2="$(unit_field "$S2" b1 pid beta)"
echo "after  -> a1:$A1_2 a2:$A2_2 b1:$B1_2"

[ -n "$A1_2" ] && [ "$A1_2" != "$A1_1" ]
check "$?" "a1 restarted (pid changed)"
[ -n "$A2_2" ] && [ "$A2_2" != "$A2_1" ]
check "$?" "a2 restarted (pid changed)"

[ "$B1_2" = "$B1_1" ]
check "$?" "b1 pid UNCHANGED by restart -p alpha"
pid_alive "$B1_1"
check "$?" "b1 process still alive (beta untouched)"

[ "$(unit_field "$S2" b1 state beta)" = "running" ]
check "$?" "beta/b1 still running in status"

sysg stop --supervisor >/dev/null 2>&1
finish
