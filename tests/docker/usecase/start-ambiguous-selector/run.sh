#!/usr/bin/env bash
# USE CASE: an ambiguous service selector must be refused, not guessed.
#
# WHAT THIS TESTS
#   Two projects (alpha, beta) each declare a service named `worker`. Starting
#   `-s worker` with NO `-p` is ambiguous -- it could mean either project's
#   worker. sysg must refuse with SG0006 (TARGET_SCOPE_AMBIGUOUS) and tell the
#   user to disambiguate with -p, rather than silently picking one. Guessing the
#   wrong project was a real prod failure class.
#
# EXPECTED OUTCOME
#   - Boot both projects (workers are skip:true so nothing is running yet).
#   - `sysg start -s worker` (no -p) exits non-zero and prints SG0006 on the
#     terminal, naming the ambiguity and pointing at -p.
#   - `sysg start -p alpha -s worker` (disambiguated) succeeds and starts only
#     alpha's worker; beta's worker stays stopped.
#   Expected RED until the one selector resolver lands.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot both projects (workers skip:true)"
sysg start --config "$CONFIG" --daemonize
check "$?" "start -c exits 0"
sleep 3

section "ambiguous -s worker (no -p) is refused with SG0006"
sysg start --service worker 2>/tmp/ambig_err.txt
RC=$?
cat /tmp/ambig_err.txt
[ "$RC" != "0" ]
check "$?" "ambiguous start -s worker exits non-zero"
stderr_has_code SG0006 /tmp/ambig_err.txt
check "$?" "stderr names SG0006 (target scope ambiguous)"

section "disambiguated -p alpha -s worker starts only alpha's worker"
sysg start --project alpha --service worker 2>/tmp/disambig_err.txt
check "$?" "start -p alpha -s worker exits 0"
# Poll one aggregate snapshot until alpha/worker is up, rather than a fixed
# sleep — the resident start settles asynchronously and a tight sleep flakes
# under load. beta/worker is read from the SAME snapshot so the two agree.
A_STATE=""; B_STATE=""
for _ in $(seq 1 15); do
  sleep 1
  S="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
  A_STATE="$(unit_field "$S" worker state alpha)"
  B_STATE="$(unit_field "$S" worker state beta)"
  [ "$A_STATE" = "running" ] && break
done
echo "alpha/worker=$A_STATE beta/worker=$B_STATE"
[ "$A_STATE" = "running" ]
check "$?" "alpha/worker is running"
[ "$B_STATE" != "running" ]
check "$?" "beta/worker stayed stopped (only alpha targeted)"

sysg stop --supervisor >/dev/null 2>&1
finish
