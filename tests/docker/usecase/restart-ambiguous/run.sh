#!/usr/bin/env bash
# USE CASE: an ambiguous restart selector must be refused, not guessed.
#
# WHAT THIS TESTS
#   Two projects (alpha, beta) each declare a service named `worker`. Restarting
#   `-s worker` with NO `-p` is ambiguous -- it could mean either project's
#   worker. sysg must refuse with SG0006 (TARGET_SCOPE_AMBIGUOUS) rather than
#   silently bouncing one. Disambiguating with `-p alpha -s worker` then
#   restarts ONLY alpha's worker, leaving beta's worker untouched. Guessing the
#   wrong project on restart was a real prod failure class.
#
# EXPECTED OUTCOME
#   - Boot both projects; both workers running; record pids.
#   - `sysg restart -s worker` (no -p) exits non-zero and prints SG0006.
#   - `sysg restart -p alpha -s worker` bounces alpha's worker (new pid) and
#     leaves beta's worker on its ORIGINAL pid, still alive.
#   Expected RED until the one selector resolver lands.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot both projects"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3
S1="$(sysg status --format json 2>/dev/null)"
ALPHA1="$(unit_field "$S1" worker pid alpha)"
BETA1="$(unit_field "$S1" worker pid beta)"
echo "before -> alpha/worker:$ALPHA1 beta/worker:$BETA1"
[ "$(unit_field "$S1" worker state alpha)" = "running" ] && \
[ "$(unit_field "$S1" worker state beta)" = "running" ]
check "$?" "both workers running before restart"

section "ambiguous -s worker (no -p) is refused with SG0006"
sysg restart --service worker 2>/tmp/ambig.txt
RC=$?
cat /tmp/ambig.txt
[ "$RC" != "0" ]
check "$?" "ambiguous restart -s worker exits non-zero"
stderr_has_code SG0006 /tmp/ambig.txt
check "$?" "stderr names SG0006 (target scope ambiguous)"

section "disambiguated -p alpha -s worker bounces only alpha's worker"
sysg restart --project alpha --service worker
check "$?" "restart -p alpha -s worker exits 0"
sleep 3
S2="$(sysg status --format json 2>/dev/null)"
ALPHA2="$(unit_field "$S2" worker pid alpha)"
BETA2="$(unit_field "$S2" worker pid beta)"
echo "after  -> alpha/worker:$ALPHA2 beta/worker:$BETA2"

[ -n "$ALPHA2" ] && [ "$ALPHA2" != "$ALPHA1" ]
check "$?" "alpha/worker restarted (pid changed)"
[ "$BETA2" = "$BETA1" ]
check "$?" "beta/worker pid UNCHANGED (only alpha targeted)"
pid_alive "$BETA1"
check "$?" "beta/worker still alive"

sysg stop --supervisor >/dev/null 2>&1
finish
