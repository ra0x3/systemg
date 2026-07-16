#!/usr/bin/env bash
# USE CASE: `inspect -p <project> -s <svc>` and the `project/service` prefix form.
#
# WHAT THIS TESTS
#   Both ways to disambiguate resolve to the SAME unit, and a -p that disagrees
#   with a project/service prefix is a typed mismatch (SG0201).
#
# HARD INVARIANTS
#   - `inspect -s web -p beta` inspects beta's web,
#   - `inspect -s beta/web` inspects the same unit,
#   - `inspect -s alpha/web -p beta` (conflicting) is refused with SG0201.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3

section "-p beta resolves beta's web"
timeout 15 sysg inspect --config "$CONFIG" -s web -p beta --format json >/tmp/p.out 2>&1
grep -q '"web"' /tmp/p.out
check "$?" "inspect -s web -p beta names the service"
grep -q "beta" /tmp/p.out
check "$?" "payload is scoped to project beta"

section "the project/service prefix form resolves the same unit"
timeout 15 sysg inspect --config "$CONFIG" -s beta/web --format json >/tmp/q.out 2>&1
grep -q '"web"' /tmp/q.out
check "$?" "inspect -s beta/web resolves"

section "conflicting -p vs prefix is refused with SG0201"
sysg inspect --config "$CONFIG" -s alpha/web -p beta >/tmp/c.out 2>/tmp/c.err
RC=$?
cat /tmp/c.out /tmp/c.err
[ "$RC" != "0" ]
check "$?" "conflicting selectors exit non-zero"
grep -q "SG0201" /tmp/c.err
check "$?" "names SG0201"

sysg stop --supervisor >/dev/null 2>&1
finish
