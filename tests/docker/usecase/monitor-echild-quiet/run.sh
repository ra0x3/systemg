#!/usr/bin/env bash
# USE CASE: the monitor does not error-spam ECHILD for a completed one-shot unit.
#
# WHAT THIS TESTS (real dogfooding bug)
#   A one-shot unit (`oneshot`, restart_policy never) runs and exits. Its child
#   is reaped, but it lingered in the monitor's process map, so every monitor
#   tick called try_wait on it, got Err(ECHILD "No child processes"), and logged
#   `ERROR ... Failed to check status of 'oneshot': No child processes (os error
#   10)` FOREVER — a log storm on a unit that simply finished. The monitor must
#   treat ECHILD as "already reaped: drop it and stop probing", quietly.
#
# EXPECTED OUTCOME
#   - oneshot completes (Done); web stays running.
#   - after several monitor ticks, the supervisor log has NO (or at most one
#     transitional) "No child processes" ERROR for oneshot — not a repeating storm.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot: a one-shot that exits + a long-running service"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
# Let the one-shot finish and several monitor ticks elapse.
sleep 8

section "the one-shot is Done, web still running"
S="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
echo "oneshot state: $(unit_field "$S" oneshot state demo)"
[ "$(unit_field "$S" web state demo)" = "running" ]
check "$?" "web is still running"

section "no ECHILD error storm in the supervisor log"
LOG="$(sysg logs --supervisor 2>/dev/null || true)"
COUNT="$(printf '%s' "$LOG" | grep -c 'No child processes' || true)"
echo "'No child processes' ERROR lines: $COUNT"
# Allow at most one transitional line; a storm is many (one per tick, ~5s window).
[ "$COUNT" -le 1 ]
check "$?" "monitor did NOT storm ECHILD errors for the reaped one-shot"

sysg stop --supervisor >/dev/null 2>&1
finish
