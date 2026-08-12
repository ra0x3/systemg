#!/usr/bin/env bash
# PARITY: start → status → stop must behave identically in user mode and
# system mode (--sys). RFC 0001 §6.1 mode-parity contract.
#
# HARD INVARIANTS
#   - both lanes boot 2 units to running,
#   - status JSON unit shape (name:state) is byte-identical across lanes,
#   - stop kills both service processes in both lanes,
#   - no lane leaks processes into the other.
set -u
. /usecase/lib.sh
. /usecase/parity-lib.sh

CONFIG=/usecase/stack.yaml

for MODE in user sys; do
  mode_begin "$MODE"

  msysg start --config "$CONFIG" --daemonize
  check "$?" "[$MODE] start exits 0"
  sleep 3

  S="$(msysg status --config "$CONFIG" --format json 2>/tmp/status.err)"
  [ "$(unit_count "$S")" = "2" ]
  check "$?" "[$MODE] status lists exactly 2 units"

  printf '%s' "$S" > /tmp/status.$MODE.json
  shape_units /tmp/status.$MODE.json > /tmp/shape.$MODE

  RUNNING="$(grep -c ':running' /tmp/shape.$MODE)"
  [ "$RUNNING" = "2" ]
  check "$?" "[$MODE] both units running"

  oracle_ok

  msysg stop --config "$CONFIG"
  check "$?" "[$MODE] stop exits 0"
  sleep 3

  SLEEPS="$(pgrep -c -x sleep || true)"
  [ "${SLEEPS:-0}" = "0" ]
  check "$?" "[$MODE] no service processes survive stop"

  teardown_oracle_ok

  mode_end
done

section "shapes match across modes"
shapes_match
check "$?" "user and sys lanes produced identical unit shapes"

finish
