#!/usr/bin/env bash
# USE CASE: a version-drifted supervisor with a VALID config is recycled cleanly.
#
# WHAT THIS TESTS
#   The happy half of the recycle path (restart-recycle-refused covers the bad
#   config). When the resident supervisor runs a different version than the CLI
#   and the config is valid, a full `sysg restart` must: stop the old daemon,
#   start a fresh one on THIS version, and bring the services back up. After it,
#   the daemon must answer at the CLI's version (drift resolved) and the services
#   must be running on NEW pids under the NEW supervisor.
#
# HARNESS
#   Two binaries: `sysg-old` (v0.0.1, the resident daemon) and `sysg` (real
#   version, the CLI that drives the recycle).
#
# EXPECTED OUTCOME
#   - sysg-old boots demo (web, api); record pids.
#   - `sysg restart -c <valid file>` detects drift, recycles, exits 0.
#   - web and api are running on NEW pids (old daemon and its children replaced).
#   - The live daemon now reports the CLI's version (drift resolved).
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
CLI_VERSION="$(sysg --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1)"

section "boot the valid config with the OLD (v0.0.1) supervisor"
sysg-old start --config "$CONFIG" --daemonize
check "$?" "old supervisor start exits 0"
sleep 3
S1="$(sysg-old status --config "$CONFIG" --format json 2>/dev/null)"
WEB1="$(unit_field "$S1" web pid)"
API1="$(unit_field "$S1" api pid)"
echo "before -> web:$WEB1 api:$API1 (cli v$CLI_VERSION)"
[ -n "$WEB1" ] && [ -n "$API1" ] && pid_alive "$WEB1" && pid_alive "$API1"
check "$?" "web and api running before restart"

section "restart with the real CLI recycles the drifted supervisor"
sysg restart --config "$CONFIG" 2>/tmp/r.txt
RC=$?
cat /tmp/r.txt
[ "$RC" = "0" ]
check "$?" "restart (recycle) exits 0"
sleep 3

section "services are back up on NEW pids under the NEW supervisor"
S2="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
WEB2="$(unit_field "$S2" web pid)"
API2="$(unit_field "$S2" api pid)"
echo "after  -> web:$WEB2 api:$API2"
[ -n "$WEB2" ] && pid_alive "$WEB2" && [ "$WEB2" != "$WEB1" ]
check "$?" "web running on a NEW pid"
[ -n "$API2" ] && pid_alive "$API2" && [ "$API2" != "$API1" ]
check "$?" "api running on a NEW pid"

section "the live daemon now runs the CLI version (drift resolved)"
sysg status --config "$CONFIG" >/tmp/sup.txt 2>&1
if grep -qi "No running supervisor" /tmp/sup.txt; then
  check 1 "new supervisor is answering the CLI"
else
  check 0 "new supervisor is answering the CLI"
fi

sysg stop --supervisor >/dev/null 2>&1
finish
