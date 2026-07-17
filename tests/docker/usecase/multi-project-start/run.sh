#!/usr/bin/env bash
# USE CASE: one config file declares many projects (projects:) plus a loose
# service, and selectors target them independently.
#
#   sysg start -c stack.yaml           -> starts everything: foo, boo, loose
#   sysg status shows units grouped by project foo / boo / (loose, no project)
#   sysg restart -p foo                -> only foo's services bounce
#   sysg restart -p boo -s shine       -> only shine in boo bounces
#   sysg stop -s loose                 -> the loose (project-less) service stops
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

# Uses lib.sh's unit_field, which reads the nested process.pid correctly.

section "start the whole file"
sysg start --config "$CONFIG" --daemonize
echo "start rc: $?"
sleep 3
STATUS="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"

section "all three units are present, grouped by their project"
[ "$(unit_field "$STATUS" bar state)" = "running" ]
check "$?" "foo/bar is running"
[ "$(unit_field "$STATUS" shine state)" = "running" ]
check "$?" "boo/shine is running"
[ "$(unit_field "$STATUS" loose state)" = "running" ]
check "$?" "loose service is running"

[ "$(unit_field "$STATUS" bar project)" = "foo" ]
check "$?" "bar is grouped under project foo"
[ "$(unit_field "$STATUS" shine project)" = "boo" ]
check "$?" "shine is grouped under project boo"

section "restart -p foo bounces only foo"
BAR_PID_1="$(unit_field "$STATUS" bar pid)"
SHINE_PID_1="$(unit_field "$STATUS" shine pid)"
sysg restart --project foo >/dev/null 2>&1
echo "restart -p foo rc: $?"
sleep 2
STATUS2="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
BAR_PID_2="$(unit_field "$STATUS2" bar pid)"
SHINE_PID_2="$(unit_field "$STATUS2" shine pid)"
[ -n "$BAR_PID_2" ] && [ "$BAR_PID_2" != "$BAR_PID_1" ]
check "$?" "foo/bar restarted (pid changed)"
[ "$SHINE_PID_2" = "$SHINE_PID_1" ]
check "$?" "boo/shine untouched by restart -p foo"

section "restart -p boo -s shine bounces only shine"
sysg restart --project boo --service shine >/dev/null 2>&1
echo "restart -p boo -s shine rc: $?"
sleep 2
STATUS3="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
SHINE_PID_3="$(unit_field "$STATUS3" shine pid)"
[ -n "$SHINE_PID_3" ] && [ "$SHINE_PID_3" != "$SHINE_PID_1" ]
check "$?" "boo/shine restarted (pid changed)"

section "stop -s loose stops the project-less service"
sysg stop --service loose >/dev/null 2>&1
echo "stop -s loose rc: $?"
sleep 2
STATUS4="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
LOOSE_STATE="$(unit_field "$STATUS4" loose state)"
echo "loose state after stop: $LOOSE_STATE"
[ "$LOOSE_STATE" != "running" ]
check "$?" "loose service is no longer running"

sysg stop --config "$CONFIG" >/dev/null 2>&1
finish
