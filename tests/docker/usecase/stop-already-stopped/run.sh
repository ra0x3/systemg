#!/usr/bin/env bash
# USE CASE: stopping an already-stopped service is an idempotent no-op.
#
# WHAT THIS TESTS
#   Stopping a service that is already stopped is an idempotent no-op that still
#   succeeds (exit 0), not an error. Callers (agents, scripts) must be able to
#   `stop` blindly without special-casing "was it already down?".
#
# EXPECTED OUTCOME
#   - After boot, `sysg stop --service svc1` exits 0 and svc1 stops.
#   - Stopping svc1 AGAIN still exits 0 (idempotent), not an error.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot both services"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3

section "first stop of svc1 takes it down"
sysg stop --service svc1
check "$?" "first stop --service svc1 exits 0"
sleep 2

STATUS="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
[ "$(unit_field "$STATUS" svc1 state)" != "running" ]
check "$?" "svc1 is no longer running"

section "stopping an already-stopped service still succeeds"
check_ok "second stop of already-stopped service still succeeds" sysg stop --service svc1

sysg stop --supervisor >/dev/null 2>&1
finish
