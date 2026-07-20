#!/usr/bin/env bash
# USE CASE: restart a target that does not exist is REFUSED, and touches nothing.
#
# WHAT THIS TESTS
#   The adversary asks systemg to restart a service or project that no config
#   declares. A supervisor that "succeeds" on a ghost target, or that bounces
#   real services while resolving a bad one, is a silent lie. Every non-existent
#   target must be refused with a typed SG0202 and leave the running set exactly
#   as it was. This mirrors the stop-path guard; restart had NO such check.
#
# EXPECTED OUTCOME
#   - Boot demo (web, api); record their pids.
#   - `restart -s ghost`      -> non-zero, SG0202, web+api untouched.
#   - `restart -p ghost`      -> non-zero, SG0202, web+api untouched.
#   - `restart -p demo -s ghost` -> non-zero, SG0202, web+api untouched.
#   - A real `restart -s web` afterward still works (guard did not wedge state).
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot the config"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3
S1="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
WEB1="$(unit_field "$S1" web pid)"
API1="$(unit_field "$S1" api pid)"
echo "before -> web:$WEB1 api:$API1"
[ -n "$WEB1" ] && [ -n "$API1" ] && pid_alive "$WEB1" && pid_alive "$API1"
check "$?" "web and api running before restart"

assert_untouched() {
  sleep 1
  pid_alive "$WEB1"; local w=$?
  pid_alive "$API1"; local a=$?
  [ "$w" = "0" ] && [ "$a" = "0" ]
}

section "restart -s ghost is refused with SG0202"
sysg restart --service ghost 2>/tmp/s.txt
RC=$?
cat /tmp/s.txt
[ "$RC" != "0" ]
check "$?" "restart -s ghost exits non-zero"
stderr_has_code SG0202 /tmp/s.txt
check "$?" "stderr names SG0202 (target not found)"
assert_untouched
check "$?" "web and api untouched after ghost service restart"

section "restart -p ghost is refused with SG0202"
sysg restart --project ghost 2>/tmp/p.txt
RC=$?
cat /tmp/p.txt
[ "$RC" != "0" ]
check "$?" "restart -p ghost exits non-zero"
stderr_has_code SG0202 /tmp/p.txt
check "$?" "stderr names SG0202 (project not found)"
assert_untouched
check "$?" "web and api untouched after ghost project restart"

section "restart -p demo -s ghost is refused with SG0202"
sysg restart --project demo --service ghost 2>/tmp/ps.txt
RC=$?
cat /tmp/ps.txt
[ "$RC" != "0" ]
check "$?" "restart -p demo -s ghost exits non-zero"
stderr_has_code SG0202 /tmp/ps.txt
check "$?" "stderr names SG0202 (service-in-project not found)"
assert_untouched
check "$?" "web and api untouched after ghost service-in-project restart"

section "a real restart still works (guard did not wedge state)"
sysg restart --service web
check "$?" "restart -s web exits 0"
sleep 2
S2="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
WEB2="$(unit_field "$S2" web pid)"
[ -n "$WEB2" ] && [ "$WEB2" != "$WEB1" ] && pid_alive "$WEB2"
check "$?" "web actually restarted (new pid, alive)"
[ "$(unit_field "$S2" api pid)" = "$API1" ]
check "$?" "api pid still unchanged (only web targeted)"

sysg stop --supervisor >/dev/null 2>&1
finish
