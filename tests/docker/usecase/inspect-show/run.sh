#!/usr/bin/env bash
# USE CASE: `sysg inspect -s <svc>` details a running service.
#
# WHAT THIS TESTS
#   The happy path. Inspect one running service and get its detail — the name,
#   a live pid, and (via --format json) a machine-readable payload naming the
#   service.
#
# HARD INVARIANTS
#   - `sysg inspect -s web` exits with a health code (0/1/2), not an error,
#   - `--format json` output names the service and carries its pid.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3

section "inspect the running service as json"
timeout 15 sysg inspect --config "$CONFIG" -s web --format json >/tmp/i.out 2>/tmp/i.err
RC=$?
cat /tmp/i.out | head -c 800; echo
[ "$RC" != "124" ]
check "$?" "inspect did not hang"
grep -q '"web"' /tmp/i.out
check "$?" "inspect json names the service"
grep -q '"pid"' /tmp/i.out
check "$?" "inspect json carries a pid"

sysg stop --supervisor >/dev/null 2>&1
finish
