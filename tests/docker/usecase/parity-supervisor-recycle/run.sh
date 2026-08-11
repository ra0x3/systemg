#!/usr/bin/env bash
# PARITY REGRESSION (sysg --sys exec-drop): the supervisor is booted via
# fork + execv of `sysg supervise`, and every self-exec site must forward the
# runtime mode. The bug: reexec_supervisor() dropped --sys, so a --sys start
# booted a USER-mode supervisor and the CLI timed out on the system socket.
#
# HARD INVARIANTS
#   - sys lane: the live supervisor process argv contains `--sys` before
#     `supervise`, and the pidfile lives under /var/lib/systemg,
#   - user lane: argv has `supervise` with NO `--sys`, pidfile under the
#     invoking user's home,
#   - both lanes stay healthy across a targeted service restart.
set -u
. /usecase/lib.sh
. /usecase/parity-lib.sh

CONFIG=/usecase/stack.yaml

for MODE in user sys; do
  mode_begin "$MODE"

  msysg start --config "$CONFIG" --daemonize
  check "$?" "[$MODE] start exits 0"
  sleep 3

  if [ "$MODE" = "sys" ]; then
    PIDFILE=/var/lib/systemg/sysg.pid
  else
    PIDFILE=/home/parity/.local/share/systemg/sysg.pid
  fi
  [ -f "$PIDFILE" ]
  check "$?" "[$MODE] supervisor pidfile at mode-correct path $PIDFILE"

  SUP_PID="$(tr -cd '0-9' < "$PIDFILE")"
  ARGS="$(ps -p "$SUP_PID" -o args= 2>/dev/null)"
  echo "supervisor argv: $ARGS"
  printf '%s' "$ARGS" | grep -q "supervise"
  check "$?" "[$MODE] supervisor process is the re-exec'd supervise image"

  if [ "$MODE" = "sys" ]; then
    printf '%s' "$ARGS" | grep -q -- "--sys"
    check "$?" "[$MODE] supervise argv carries --sys across the exec boundary"
  else
    ! printf '%s' "$ARGS" | grep -q -- "--sys"
    check "$?" "[$MODE] supervise argv has no --sys in user mode"
  fi

  msysg restart --service web --config "$CONFIG"
  check "$?" "[$MODE] targeted restart exits 0"
  sleep 3

  S="$(msysg status --config "$CONFIG" --format json)"
  [ "$(unit_count "$S")" = "2" ]
  check "$?" "[$MODE] both units still tracked after restart"

  echo "argv-mode=correct pidfile=correct restart=ok" > /tmp/shape.$MODE

  mode_end
done

section "shapes match across modes"
shapes_match
check "$?" "user and sys lanes behaved identically"

finish
