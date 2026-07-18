#!/usr/bin/env bash
# USE CASE: inspecting a non-existent service is refused with SG0202.
#
# WHAT THIS TESTS
#   The typed not-found path (was a bare "Service 'x' not found." + exit 2).
#   Inspecting a service the supervisor has no record of must be a typed
#   diagnostic naming the service.
#
# HARD INVARIANTS
#   - `sysg inspect -s ghost` exits non-zero and names SG0202,
#   - a real `inspect -s web` still works (guard against false positives).
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3

section "inspecting a ghost service is refused with SG0202"
sysg inspect --config "$CONFIG" -s ghost >/tmp/g.out 2>/tmp/g.err
RC=$?
cat /tmp/g.out /tmp/g.err
[ "$RC" != "0" ]
check "$?" "inspect of a missing service exits non-zero"
grep -q "SG0202" /tmp/g.err
check "$?" "names SG0202"
grep -q "ghost" /tmp/g.err
check "$?" "names the missing service"

section "a real service still inspects fine"
timeout 15 sysg inspect --config "$CONFIG" -s web --format json >/tmp/w.out 2>&1
grep -q '"web"' /tmp/w.out
check "$?" "inspect of a real service is not falsely refused"

sysg stop --supervisor >/dev/null 2>&1
finish
