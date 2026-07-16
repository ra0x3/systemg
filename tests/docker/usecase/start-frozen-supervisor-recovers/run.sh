#!/usr/bin/env bash
# USE CASE: `start --daemonize` recovers when the resident supervisor is alive
# but not answering (frozen / mid-shutdown), instead of routing into it.
#
# WHAT THIS TESTS
#   BUG-4: supervisor_running() trusts a live pid, so a daemon whose process is
#   alive but whose control socket is dead (SIGSTOP here, a shutdown/wedge in
#   prod) is mis-reported as usable. A start that routes into it hangs or is
#   dropped ("supervisor dropped the command before replying" — the prod pain
#   that forced `sysg purge`). The preflight health probe must see the daemon is
#   NOT serving, clear the stale runtime, and fork a fresh supervisor.
#
# EXPECTED OUTCOME
#   - Boot demo; record supervisor pid + web pid.
#   - SIGSTOP the supervisor: pid stays alive, socket goes unresponsive.
#   - `start --daemonize` recovers: exits 0, does NOT hang, boots a new
#     supervisor whose status answers and shows web running.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
STATE_DIR="$HOME/.local/share/systemg"

section "boot the config"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3
SUP1="$(cat "$STATE_DIR/sysg.pid")"
echo "supervisor pid: $SUP1"
[ -n "$SUP1" ] && pid_alive "$SUP1"
check "$?" "supervisor process alive"

section "freeze the supervisor (alive pid, dead socket)"
kill -STOP "$SUP1"
pid_alive "$SUP1"
check "$?" "frozen supervisor pid is STILL alive (would mis-report as running)"

section "start --daemonize recovers instead of hanging on the frozen daemon"
timeout 30 sysg start --config "$CONFIG" --daemonize 2>/tmp/rec.txt
RC=$?
cat /tmp/rec.txt
[ "$RC" != "124" ]
check "$?" "recovery start did NOT hang (no timeout)"
[ "$RC" = "0" ]
check "$?" "recovery start exits 0"

sleep 3
SUP2="$(cat "$STATE_DIR/sysg.pid")"
echo "new supervisor pid: $SUP2"
[ -n "$SUP2" ] && [ "$SUP2" != "$SUP1" ]
check "$?" "a NEW supervisor was forked (pid changed)"

STATUS="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
[ "$(unit_field "$STATUS" web state)" = "running" ]
check "$?" "web running under the fresh supervisor"

kill -CONT "$SUP1" 2>/dev/null
kill -9 "$SUP1" 2>/dev/null
sysg stop --supervisor >/dev/null 2>&1
finish
