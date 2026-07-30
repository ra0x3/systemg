#!/usr/bin/env bash
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

sysg start -c "$CONFIG" --daemonize
check "$?" "cron project starts"
sleep 2
sysg restart -p demo -s job >/tmp/restart.out 2>/tmp/restart.err
RC=$?
cat /tmp/restart.err
[ "$RC" != "0" ]
check "$?" "direct cron restart is rejected"
stderr_has_code SG0101 /tmp/restart.err
check "$?" "direct cron restart uses its specific code"
grep -q "run only when their schedule fires" /tmp/restart.err
check "$?" "diagnostic explains schedule-driven control"
! grep -q "restart -s job" /tmp/restart.err
check "$?" "diagnostic never recommends the rejected command"

sysg stop --supervisor >/dev/null 2>&1
finish
