#!/usr/bin/env bash
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

sysg start -c "$CONFIG" --daemonize
check "$?" "service starts after successful finite dependency"
sleep 2
STATUS="$(sysg status -p demo --format json 2>/dev/null)"
OLD="$(unit_field "$STATUS" web pid demo)"
pid_alive "$OLD"
check "$?" "dependent service is initially live"
[ "$(wc -l < /tmp/ready-runs | tr -d ' ')" = "1" ]
check "$?" "finite prerequisite ran once"

kill -9 "$OLD"
NEW=""
for _ in $(seq 1 12); do
  sleep 1
  STATUS="$(sysg status -p demo --format json 2>/dev/null)"
  NEW="$(unit_field "$STATUS" web pid demo)"
  if [ "$NEW" != "$OLD" ] && pid_alive "$NEW"; then
    break
  fi
done
[ "$NEW" != "$OLD" ] && pid_alive "$NEW"
check "$?" "automatic recovery accepts the completed prerequisite"
[ "$(wc -l < /tmp/ready-runs | tr -d ' ')" = "1" ]
check "$?" "recovery does not rerun the satisfied prerequisite"

sysg stop --supervisor >/dev/null 2>&1
finish
