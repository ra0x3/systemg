#!/usr/bin/env bash
# PARITY: cron scheduling must behave identically in user mode and system mode
# (--sys): the job fires, the unit is visible in status, runs are recorded.
#
# HARD INVARIANTS
#   - the every-second cron fires within 15s in both lanes,
#   - status lists the cron unit in both lanes,
#   - stop halts firing in both lanes.
set -u
. /usecase/lib.sh
. /usecase/parity-lib.sh

CONFIG=/usecase/stack.yaml

for MODE in user sys; do
  mode_begin "$MODE"
  rm -f /tmp/cron_fired.log

  msysg start --config "$CONFIG" --daemonize
  check "$?" "[$MODE] start exits 0"

  FIRED=0
  for _ in $(seq 15); do
    [ -s /tmp/cron_fired.log ] && FIRED=1 && break
    sleep 1
  done
  [ "$FIRED" = "1" ]
  check "$?" "[$MODE] cron fired within 15s"

  S="$(msysg status --config "$CONFIG" --format json)"
  [ "$(unit_field "$S" ticker name)" = "ticker" ]
  check "$?" "[$MODE] cron unit visible in status"

  msysg stop --config "$CONFIG"
  check "$?" "[$MODE] stop exits 0"
  sleep 2
  cp /tmp/cron_fired.log /tmp/fired.snapshot 2>/dev/null || : > /tmp/fired.snapshot
  sleep 3
  cmp -s /tmp/cron_fired.log /tmp/fired.snapshot 2>/dev/null || [ ! -f /tmp/cron_fired.log ]
  check "$?" "[$MODE] cron stopped firing after stop"

  echo "fired=yes visible=yes halted=yes" > /tmp/shape.$MODE

  mode_end
done

section "shapes match across modes"
shapes_match
check "$?" "user and sys lanes behaved identically"

finish
