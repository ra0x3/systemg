#!/usr/bin/env bash
# USE CASE: `skip: true` is honored — a skipped service does not run.
#
# WHAT THIS TESTS (0.54.2 bug: skip ignored)
#   A service with skip: true must NOT be started; its siblings run normally.
#
# HARD INVARIANTS
#   - the skipped service has NO process and emits no output,
#   - status shows it as skipped (not running),
#   - the normal sibling runs fine.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "start (one service is skip: true)"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3

section "the skipped service did NOT run"
[ "$(pgrep -f 'echo SKIPPED_LINE' | wc -l | tr -d ' ')" = "0" ]
check "$?" "no process for the skipped service"
! sysg logs --config "$CONFIG" -p dev -s skipped --no-follow 2>/dev/null | grep -q "SKIPPED_LINE"
check "$?" "the skipped service produced no output"

section "status shows it skipped, not running"
S="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
SK="$(unit_field "$S" skipped state dev)"
echo "skipped -> state:$SK"
[ "$SK" != "running" ]
check "$?" "status does not show the skipped service as running"

section "the normal sibling runs"
NM="$(unit_field "$S" normal pid dev)"
[ -n "$NM" ] && [ "$NM" != "absent" ] && pid_alive "$NM"
check "$?" "the normal service is running on a live pid"

sysg stop --supervisor >/dev/null 2>&1
finish
