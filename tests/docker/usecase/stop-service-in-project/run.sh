#!/usr/bin/env bash
# USE CASE: stopping one service qualified by its project targets exactly it.
#
# WHAT THIS TESTS
#   Project `shop` runs two services, `api` and `worker`. Stopping worker by its
#   fully-qualified selector -- both the `-p shop -s worker` form and the
#   combined `shop/worker` form -- must stop ONLY worker. api must keep running
#   with the same pid; nothing else may be touched.
#
# EXPECTED OUTCOME
#   - After boot, both api and worker are running under project shop.
#   - `sysg stop -p shop -s worker` exits 0; worker stops, api still runs with
#     its original (unchanged, alive) pid.
#   - After restarting worker, `sysg stop -s shop/worker` stops it again.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot the shop project (api + worker)"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3

STATUS="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
[ "$(unit_field "$STATUS" api state shop)" = "running" ]
check "$?" "api is running"
[ "$(unit_field "$STATUS" worker state shop)" = "running" ]
check "$?" "worker is running"
API_PID="$(unit_field "$STATUS" api pid shop)"
echo "api pid: $API_PID"

section "stop -p shop -s worker stops only worker"
sysg stop --project shop --service worker
check "$?" "stop -p shop -s worker exits 0"
sleep 2

STATUS2="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
[ "$(unit_field "$STATUS2" worker state shop)" != "running" ]
check "$?" "worker is stopped"
[ "$(unit_field "$STATUS2" api state shop)" = "running" ]
check "$?" "api is still running (only worker targeted)"
[ "$(unit_field "$STATUS2" api pid shop)" = "$API_PID" ]
check "$?" "api pid is unchanged"
pid_alive "$API_PID"
check "$?" "api process is still alive"

section "restart worker, then stop via the shop/worker selector"
sysg start --project shop --service worker >/dev/null 2>&1
sleep 2

sysg stop --service shop/worker
check "$?" "stop -s shop/worker exits 0"
sleep 2
STATUS3="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
[ "$(unit_field "$STATUS3" worker state shop)" != "running" ]
check "$?" "worker is stopped again via the combined selector"

sysg stop --supervisor >/dev/null 2>&1
finish
