#!/usr/bin/env bash
# USE CASE: a bare `-s web` matching two projects is refused with SG0006.
#
# WHAT THIS TESTS
#   Two projects (alpha, beta) each declare a `web`. A bare `inspect -s web`
#   cannot know which one you mean — it must refuse with SG0006 (ambiguous
#   scope) rather than silently pick one. Qualifying with -p resolves it.
#
# HARD INVARIANTS
#   - `inspect -s web` (both projects) exits non-zero with SG0006,
#   - `inspect -s web -p alpha` resolves and inspects alpha's web.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot the two-project stack (both declare 'web')"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3

section "bare -s web is ambiguous across projects -> SG0006"
sysg inspect --config "$CONFIG" -s web >/tmp/a.out 2>/tmp/a.err
RC=$?
cat /tmp/a.out /tmp/a.err
[ "$RC" != "0" ]
check "$?" "ambiguous inspect exits non-zero"
grep -q "SG0006" /tmp/a.err
check "$?" "names SG0006 (ambiguous scope)"

section "qualifying with -p resolves it"
timeout 15 sysg inspect --config "$CONFIG" -s web -p alpha --format json >/tmp/b.out 2>/tmp/b.err
RC=$?
cat /tmp/b.out | head -c 400; echo
[ "$RC" != "124" ]
check "$?" "project-qualified inspect did not hang"
grep -q '"web"' /tmp/b.out
check "$?" "inspect -p alpha resolves to a single web"

sysg stop --supervisor >/dev/null 2>&1
finish
