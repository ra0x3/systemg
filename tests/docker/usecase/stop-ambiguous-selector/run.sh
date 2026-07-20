#!/usr/bin/env bash
# USE CASE: an ambiguous stop selector must be refused, not guessed.
#
# WHAT THIS TESTS
#   Two projects (alpha, beta) each declare a service named `worker`. Stopping
#   `-s worker` with NO `-p` is ambiguous -- it could mean either project's
#   worker. sysg must refuse with SG0006 (TARGET_SCOPE_AMBIGUOUS) and tell the
#   user to disambiguate with -p, rather than silently picking one. Guessing the
#   wrong project was a real prod failure class.
#
# EXPECTED OUTCOME
#   - Boot both projects; both workers are running.
#   - `sysg stop -s worker` (no -p) exits non-zero and prints SG0006 on the
#     terminal, naming the ambiguity and pointing at -p.
#   - `sysg stop -p alpha -s worker` (disambiguated) succeeds and stops only
#     alpha's worker; beta's worker stays running.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot both projects (both workers running)"
sysg start --config "$CONFIG" --daemonize
check "$?" "start -c exits 0"
sleep 3

ALPHA="$(sysg status --project alpha --format json 2>/dev/null)"
BETA="$(sysg status --project beta --format json 2>/dev/null)"
[ "$(unit_field "$ALPHA" worker state alpha)" = "running" ]
check "$?" "alpha/worker is running"
[ "$(unit_field "$BETA" worker state beta)" = "running" ]
check "$?" "beta/worker is running"

section "ambiguous -s worker (no -p) is refused with SG0006"
sysg stop --service worker 2>/tmp/ambig_err.txt
RC=$?
cat /tmp/ambig_err.txt
[ "$RC" != "0" ]
check "$?" "ambiguous stop -s worker exits non-zero"
stderr_has_code SG0006 /tmp/ambig_err.txt
check "$?" "stderr names SG0006 (target scope ambiguous)"

section "disambiguated -p alpha -s worker stops only alpha's worker"
sysg stop --project alpha --service worker
check "$?" "stop -p alpha -s worker exits 0"
sleep 2
ALPHA2="$(sysg status --project alpha --format json 2>/dev/null)"
BETA2="$(sysg status --project beta --format json 2>/dev/null)"
[ "$(unit_field "$ALPHA2" worker state alpha)" != "running" ]
check "$?" "alpha/worker is stopped"
[ "$(unit_field "$BETA2" worker state beta)" = "running" ]
check "$?" "beta/worker stayed running (only alpha targeted)"

sysg stop --supervisor >/dev/null 2>&1
finish
