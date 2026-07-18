#!/usr/bin/env bash
# USE CASE: logs are isolated per project — the arbitration/gamecast blindness.
#
# WHAT THIS TESTS
#   The prod bug: two projects each with a service named `web` shared one
#   `web.log`, so `logs -p arbitration` returned gamecast's lines. Logs are now
#   keyed by {project}/{service}, so each project's logs are its own. Also locks
#   the strictness: bare `logs` is refused, `--supervisor` is the only no-project
#   form, and a loose `-s` miss is SG0021.
#
# HARD INVARIANTS
#   - two projects (alpha, beta) each declare a service `web`,
#   - `logs -p alpha` shows ONLY ALPHA_LINE, never BETA_LINE (and vice versa),
#   - a loose service reads __loose__; `logs -s <missing>` (no -p) is SG0021,
#   - bare `logs` is refused SG0019; `logs --supervisor -s x` is SG0020,
#   - `logs --supervisor` shows the supervisor log.
set -u
. /usecase/lib.sh

section "boot two projects (both declare 'web') + a loose service"
sysg start --config /usecase/alpha.yaml --daemonize
check "$?" "alpha start exits 0"
sysg start --config /usecase/beta.yaml --daemonize
check "$?" "beta start exits 0"
sysg start --config /usecase/loose.yaml --daemonize
check "$?" "loose start exits 0"
sleep 5

section "logs -p alpha shows ONLY alpha's output"
A="$(sysg logs --config /usecase/alpha.yaml -p alpha --no-follow 2>/dev/null)"
echo "$A" | grep -q "ALPHA_LINE"
check "$?" "logs -p alpha contains ALPHA_LINE"
! echo "$A" | grep -q "BETA_LINE"
check "$?" "logs -p alpha does NOT contain BETA_LINE (isolation)"

section "logs -p beta shows ONLY beta's output"
B="$(sysg logs --config /usecase/beta.yaml -p beta --no-follow 2>/dev/null)"
echo "$B" | grep -q "BETA_LINE"
check "$?" "logs -p beta contains BETA_LINE"
! echo "$B" | grep -q "ALPHA_LINE"
check "$?" "logs -p beta does NOT contain ALPHA_LINE (isolation)"

section "on-disk: separate per-project log dirs"
LOGDIR="$HOME/.local/share/systemg/logs"
[ -d "$LOGDIR/alpha" ] && [ -d "$LOGDIR/beta" ] && [ -d "$LOGDIR/__loose__" ]
check "$?" "logs/{alpha,beta,__loose__} dirs exist"

section "loose service reads __loose__; a loose miss is SG0021"
sysg logs --config /usecase/loose.yaml -s loosesvc --no-follow 2>/dev/null | grep -q "LOOSE_LINE"
check "$?" "logs -s loosesvc reads the loose bundle"
sysg logs --config /usecase/loose.yaml -s ghostsvc >/tmp/g.err 2>&1
grep -q "SG0021" /tmp/g.err
check "$?" "logs -s <missing> (no -p) is SG0021"

section "strictness: bare logs refused; --supervisor rules"
sysg logs >/tmp/b.err 2>&1
grep -q "SG0019" /tmp/b.err
check "$?" "bare logs is refused with SG0019"
sysg logs --supervisor -s web >/tmp/c.err 2>&1
grep -q "SG0020" /tmp/c.err
check "$?" "--supervisor with a selector is SG0020"
sysg logs --supervisor --no-follow 2>/tmp/s.err | grep -qi "supervisor" || grep -qi "supervisor" /tmp/s.err
check "$?" "logs --supervisor shows the supervisor log"

sysg stop --supervisor >/dev/null 2>&1
finish
