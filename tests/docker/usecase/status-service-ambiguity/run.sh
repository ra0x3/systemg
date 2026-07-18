#!/usr/bin/env bash
# USE CASE: an ambiguous `status -s` across a multi-project config is refused.
#
# WHAT THIS TESTS
#   The abuse case: a config declares `web` in BOTH alpha and beta. `status -s web`
#   with no `-p` is genuinely ambiguous within that resolved config, so status —
#   read-only though it is — keeps the same selector contract as start/stop/
#   restart and refuses with SG0006 rather than guessing. Disambiguating with
#   `-p` resolves it. (A single-project config makes `-s web` unambiguous; that is
#   covered by other cases — here every path is the multi-project abuse.)
#
# EXPECTED OUTCOME
#   - Boot alpha (web, worker) + beta (web, worker).
#   - `status -s web`            -> non-zero, SG0006 (ambiguous), no unit table trusted.
#   - `status -p alpha -s web`   -> exit reflects health, shows exactly alpha/web.
#   - `status -s worker`         -> also SG0006 (worker is in both too).
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot both projects (each declares web + worker)"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3

section "status -s web (no -p) is refused with SG0006"
sysg status --config "$CONFIG" --service web --format json >/tmp/out.txt 2>/tmp/err.txt
RC=$?
echo "rc=$RC"; cat /tmp/err.txt
[ "$RC" != "0" ]
check "$?" "ambiguous status -s web exits non-zero"
stderr_has_code SG0006 /tmp/err.txt
check "$?" "stderr names SG0006 (target scope ambiguous)"
grep -qi "alpha" /tmp/err.txt && grep -qi "beta" /tmp/err.txt
check "$?" "SG0006 names both projects (alpha, beta)"

section "status -s worker (no -p) is also refused with SG0006"
sysg status --config "$CONFIG" --service worker 2>/tmp/err2.txt
RC=$?
[ "$RC" != "0" ]
check "$?" "ambiguous status -s worker exits non-zero"
stderr_has_code SG0006 /tmp/err2.txt
check "$?" "stderr names SG0006 for worker too"

section "status -p alpha -s web disambiguates to exactly alpha/web"
SAW="$(sysg status --config "$CONFIG" --project alpha --service web --format json 2>/tmp/err3.txt)"
stderr_has_code SG0006 /tmp/err3.txt && AMBIG=1 || AMBIG=0
[ "$AMBIG" = "0" ]
check "$?" "disambiguated -p alpha -s web is NOT refused"
[ "$(unit_count "$SAW")" = "1" ]
check "$?" "shows exactly 1 unit"
[ "$(unit_field "$SAW" web pid alpha)" != "absent" ]
check "$?" "the single unit is alpha/web"

sysg stop --supervisor >/dev/null 2>&1
finish
