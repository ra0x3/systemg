#!/usr/bin/env bash
# USE CASE: restart with NO supervisor running falls back to a clean start.
#
# WHAT THIS TESTS
#   `sysg restart --daemonize -c <file>` when nothing is running must not error
#   out about a missing supervisor -- it boots a fresh supervisor cleanly, the
#   same as `start` would. restart is the one command an agent can always reach
#   for; a cold restart has to just work.
#
# EXPECTED OUTCOME
#   - No prior start. `sysg restart -c <file> --daemonize` exits 0.
#   - web comes up running with a live pid.
#   - stderr does NOT surface a "No running supervisor" error.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "restart with no supervisor boots a fresh one"
sysg restart --config "$CONFIG" --daemonize 2>/tmp/r.txt
check "$?" "restart -c --daemonize exits 0 (cold)"
cat /tmp/r.txt
sleep 3

S1="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
WEB="$(unit_field "$S1" web pid)"
echo "web -> pid:$WEB state:$(unit_field "$S1" web state)"
[ "$(unit_field "$S1" web state)" = "running" ]
check "$?" "web is running after cold restart"
[ -n "$WEB" ] && pid_alive "$WEB"
check "$?" "web pid is alive"

if grep -qi "No running supervisor" /tmp/r.txt; then
  check 1 "no 'No running supervisor' error on cold restart"
else
  check 0 "no 'No running supervisor' error on cold restart"
fi

sysg stop --supervisor >/dev/null 2>&1
finish
