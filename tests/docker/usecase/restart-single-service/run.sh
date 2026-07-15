#!/usr/bin/env bash
# USE CASE: restart -s <service> bounces only that one service.
#
# WHAT THIS TESTS
#   Project `demo` with two services, web and api. `sysg restart --service web`
#   must give web a NEW pid (and bring it back up) while leaving api completely
#   alone — same pid, still alive. A single-service restart is surgical: it
#   touches exactly the named service and nothing else.
#
# EXPECTED OUTCOME
#   - After restart -s web: web has a NEW pid, is running, and that pid is alive.
#   - api's pid is UNCHANGED and its process is still alive (untouched).
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot the two-service project"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3
S1="$(sysg status --format json 2>/dev/null)"
WEB_1="$(unit_field "$S1" web pid demo)"
API_1="$(unit_field "$S1" api pid demo)"
echo "before -> web:$WEB_1 api:$API_1"

section "restart -s web bounces web only"
sysg restart --service web
check "$?" "restart -s web exits 0"
sleep 3
S2="$(sysg status --format json 2>/dev/null)"
WEB_2="$(unit_field "$S2" web pid demo)"
API_2="$(unit_field "$S2" api pid demo)"
echo "after  -> web:$WEB_2 api:$API_2"

[ -n "$WEB_2" ] && [ "$WEB_2" != "$WEB_1" ]
check "$?" "web restarted (pid changed)"
[ "$(unit_field "$S2" web state demo)" = "running" ]
check "$?" "web is running after restart"
pid_alive "$WEB_2"
check "$?" "web's new pid is actually alive"

[ "$API_2" = "$API_1" ]
check "$?" "api pid UNCHANGED by restart -s web"
pid_alive "$API_1"
check "$?" "api process still alive (untouched)"

sysg stop --supervisor >/dev/null 2>&1
finish
