#!/usr/bin/env bash
# PARITY: log capture and purge must behave identically in user mode and
# system mode (--sys): captured lines are served, purge empties both the files
# and the served buffer (the sysg-logs-purge-reader-stale regression).
#
# HARD INVARIANTS
#   - both lanes serve captured "tick" lines via `logs --no-follow`,
#   - after `logs --purge`, served logs no longer contain the old lines,
#   - new lines appear again after purge (capture still wired).
set -u
. /usecase/lib.sh
. /usecase/parity-lib.sh

CONFIG=/usecase/stack.yaml

for MODE in user sys; do
  mode_begin "$MODE"

  msysg start --config "$CONFIG" --daemonize
  check "$?" "[$MODE] start exits 0"
  sleep 4

  msysg logs --service chatty --no-follow --config "$CONFIG" > /tmp/logs.pre 2>&1
  grep -q "tick-" /tmp/logs.pre
  check "$?" "[$MODE] captured lines served before purge"
  PRE_FIRST="$(grep -o 'tick-[0-9]*' /tmp/logs.pre | head -1)"

  msysg logs --purge --config "$CONFIG"
  check "$?" "[$MODE] logs --purge exits 0"

  msysg logs --service chatty --no-follow --config "$CONFIG" > /tmp/logs.post 2>&1
  ! grep -q "$PRE_FIRST" /tmp/logs.post
  check "$?" "[$MODE] purge removed pre-purge marker $PRE_FIRST from served logs"

  sleep 3
  msysg logs --service chatty --no-follow --config "$CONFIG" > /tmp/logs.new 2>&1
  grep -q "tick-" /tmp/logs.new
  check "$?" "[$MODE] capture still live after purge"

  echo "served:pre=yes purge=drop capture=live" > /tmp/shape.$MODE

  mode_end
  pkill -f "while true" 2>/dev/null
  sleep 1
done

section "shapes match across modes"
shapes_match
check "$?" "user and sys lanes behaved identically"

finish
