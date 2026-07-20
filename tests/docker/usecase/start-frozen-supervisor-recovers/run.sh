#!/usr/bin/env bash
# USE CASE: `start --daemonize` refuses a resident supervisor that is alive but
# not answering instead of risking duplicate supervision.
#
# WHAT THIS TESTS
#   BUG-4: supervisor_running() trusts a live pid, so a daemon whose process is
#   alive but whose control socket is dead (SIGSTOP here, a shutdown/wedge in
#   prod) must not receive commands or be replaced while its workloads remain
#   alive. The preflight health probe must refuse with SG0205.
#
# EXPECTED OUTCOME
#   - Boot demo; record supervisor pid + web pid.
#   - SIGSTOP the supervisor: pid stays alive, socket goes unresponsive.
#   - `start --daemonize` returns promptly with SG0205, preserves the resident
#     supervisor identity, and leaves its existing web process alive.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
STATE_DIR="$HOME/.local/share/systemg"

section "boot the config"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3
SUP1="$(cat "$STATE_DIR/sysg.pid")"
STATUS1="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
WEB1="$(unit_field "$STATUS1" web pid)"
echo "supervisor pid: $SUP1"
[ -n "$SUP1" ] && pid_alive "$SUP1" && pid_alive "$WEB1"
check "$?" "supervisor and web process are alive"

section "freeze the supervisor (alive pid, dead socket)"
kill -STOP "$SUP1"
pid_alive "$SUP1"
check "$?" "frozen supervisor pid is STILL alive (would mis-report as running)"

section "start --daemonize refuses instead of duplicating the frozen daemon"
timeout 30 sysg start --config "$CONFIG" --daemonize 2>/tmp/rec.txt
RC=$?
cat /tmp/rec.txt
[ "$RC" != "124" ]
check "$?" "recovery start did NOT hang (no timeout)"
[ "$RC" != "0" ] && stderr_has_code SG0205 /tmp/rec.txt
check "$?" "start refuses with SG0205"

SUP2="$(cat "$STATE_DIR/sysg.pid")"
echo "supervisor pid after refusal: $SUP2"
[ "$SUP2" = "$SUP1" ] && pid_alive "$SUP1"
check "$?" "the resident supervisor identity is preserved"
pid_alive "$WEB1"
check "$?" "the existing web process remains alive"

kill -CONT "$SUP1" 2>/dev/null
kill -9 "$SUP1" 2>/dev/null
sysg stop --supervisor >/dev/null 2>&1
finish
