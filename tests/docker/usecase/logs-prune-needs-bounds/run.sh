#!/usr/bin/env bash
# USE CASE: `logs --prune` with no bound is refused with SG0017.
#
# WHAT THIS TESTS
#   Prune trims rotated backups down to a size/age bound. With neither bound
#   there is nothing to prune against — a user error that must be a typed
#   diagnostic (SG0017), not a silent no-op or a bare message. With a bound it
#   must run.
#
# HARD INVARIANTS
#   - `sysg logs --prune` (no bound) exits non-zero and names SG0017,
#   - `sysg logs --prune --max-size 500MB` exits 0.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot so a log dir exists"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 2

section "prune with no bound is refused with SG0017"
sysg logs --config "$CONFIG" --prune >/tmp/p.out 2>/tmp/p.err
RC=$?
cat /tmp/p.out /tmp/p.err
[ "$RC" != "0" ]
check "$?" "prune with no bound exits non-zero"
grep -q "SG0017" /tmp/p.err
check "$?" "refusal names SG0017"

section "prune with a bound runs"
sysg logs --config "$CONFIG" --prune --max-size 500MB >/tmp/p2.out 2>/tmp/p2.err
RC=$?
cat /tmp/p2.out /tmp/p2.err
[ "$RC" = "0" ]
check "$?" "prune --max-size exits 0"

sysg stop --supervisor >/dev/null 2>&1
finish
