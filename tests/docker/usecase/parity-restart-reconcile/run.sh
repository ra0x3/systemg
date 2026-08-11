#!/usr/bin/env bash
# PARITY: targeted service restart must behave identically in user mode and
# system mode (--sys): same scope (only the target bounces), same shape after.
#
# HARD INVARIANTS
#   - restart -s web replaces web's PID and leaves api's PID untouched,
#   - post-restart status shape is identical across lanes,
#   - exactly 2 service processes before and after in both lanes.
set -u
. /usecase/lib.sh
. /usecase/parity-lib.sh

CONFIG=/usecase/stack.yaml

for MODE in user sys; do
  mode_begin "$MODE"

  msysg start --config "$CONFIG" --daemonize
  check "$?" "[$MODE] start exits 0"
  sleep 3

  S="$(msysg status --config "$CONFIG" --format json)"
  WEB_PID="$(unit_field "$S" web pid)"
  API_PID="$(unit_field "$S" api pid)"
  pid_alive "$WEB_PID" && pid_alive "$API_PID"
  check "$?" "[$MODE] both units alive pre-restart"

  msysg restart --service web --config "$CONFIG"
  check "$?" "[$MODE] restart -s web exits 0"
  sleep 3

  S2="$(msysg status --config "$CONFIG" --format json)"
  WEB_PID2="$(unit_field "$S2" web pid)"
  API_PID2="$(unit_field "$S2" api pid)"

  [ "$WEB_PID2" != "$WEB_PID" ] && pid_alive "$WEB_PID2"
  check "$?" "[$MODE] web has a new live PID"

  [ "$API_PID2" = "$API_PID" ] && pid_alive "$API_PID2"
  check "$?" "[$MODE] api PID untouched by sibling restart"

  SLEEPS="$(pgrep -c -x sleep || true)"
  [ "${SLEEPS:-0}" = "2" ]
  check "$?" "[$MODE] exactly 2 service processes after restart"

  printf '%s' "$S2" > /tmp/status.$MODE.json
  shape_units /tmp/status.$MODE.json > /tmp/shape.$MODE

  mode_end
done

section "shapes match across modes"
shapes_match
check "$?" "user and sys lanes produced identical unit shapes"

finish
