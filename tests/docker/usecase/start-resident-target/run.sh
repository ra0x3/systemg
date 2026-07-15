#!/usr/bin/env bash
# USE CASE: start a service/project by id against an ALREADY-RUNNING supervisor,
# without a config file on hand.
#
# WHAT THIS TESTS
#   Once a supervisor is resident, `sysg start -p <id>` and
#   `sysg start -p <id> -s <svc>` must target the running project by id and
#   NOT read a manifest from disk. This is the core selector invariant: a
#   resident target is resolved from the supervisor, never from ./systemg.yaml.
#   We prove "no file read" by running the by-id starts from a scratch working
#   directory that contains no config, with no -c flag.
#
# EXPECTED OUTCOME
#   - Initial `start -c` boots project `shop` with `api` running; `worker` is
#     skip:true so it starts stopped.
#   - From an empty dir with no -c: `sysg start -p shop -s worker` starts the
#     previously-skipped worker by id, and it does NOT fail with a
#     config-read/"No such file" error.
#   - status then shows worker running under shop.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot the project from its file (worker is skip:true)"
sysg start --config "$CONFIG" --daemonize
check "$?" "start -c exits 0"
sleep 3
STATUS="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
[ "$(unit_field "$STATUS" api state)" = "running" ]
check "$?" "api is running after boot"
[ "$(unit_field "$STATUS" worker state)" != "running" ]
check "$?" "worker is not running yet (skip:true)"

section "start the worker BY ID from an empty dir (no -c, no local config)"
mkdir -p /tmp/empty
cd /tmp/empty || exit 1
sysg start --project shop --service worker 2>/tmp/resident_err.txt
RC=$?
cat /tmp/resident_err.txt
[ "$RC" = "0" ]
check "$?" "start -p shop -s worker exits 0 with no config on disk"
if grep -qiE "No such file|Failed to read config|systemg.yaml" /tmp/resident_err.txt; then
  check 1 "by-id start did NOT try to read a local config file"
else
  check 0 "by-id start did NOT try to read a local config file"
fi

section "status now shows worker running under shop"
sleep 2
STATUS2="$(sysg status --project shop --format json 2>/dev/null)"
[ "$(unit_field "$STATUS2" worker state shop)" = "running" ]
check "$?" "worker is running under project shop after by-id start"

sysg stop --supervisor >/dev/null 2>&1
finish
