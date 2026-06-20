#!/usr/bin/env bash
# Reproduces the systemg zombie-wrapper bug: service wrapper shells become
# unreaped [sh] <defunct> children of the sysg daemon, causing `sysg status`
# to report healthy long-running services as Zombie/Failing while the real
# workers keep running (orphaned under PID 1, same PGID).
set -u

CONFIG=/repro/sysg.config.yaml
export HOME=/root

section() { printf '\n========== %s ==========\n' "$1"; }

section "starting sysg daemon (this process is PID $$, acts as the supervisor parent)"
sysg start --config "$CONFIG" --log-level debug --daemonize
sleep 8

DAEMON_PID="$(pgrep -f 'sysg start' | head -1)"
echo "sysg daemon PID: ${DAEMON_PID:-<none>}"

section "process tree (look for [sh] <defunct> owned by the daemon)"
ps -eo pid,ppid,pgid,stat,comm,args | grep -E "sysg|sleep|sh|PID" | grep -v grep

section "recorded pid.xml"
cat "$HOME/.local/share/systemg/pid.xml" 2>/dev/null || echo "no pid.xml"

section "sysg status"
sysg status --config "$CONFIG"

section "BUG CHECK"
ZOMBIES="$(ps -eo ppid,stat,comm | awk -v d="${DAEMON_PID:-0}" '$1==d && $2 ~ /Z/ {print}')"
LIVE_SLEEPERS="$(pgrep -x sleep | wc -l | tr -d ' ')"

if [ -n "$ZOMBIES" ]; then
  echo "REPRODUCED: daemon $DAEMON_PID has zombie children:"
  echo "$ZOMBIES"
else
  echo "no zombie children under the daemon"
fi
echo "live 'sleep' workers still running: $LIVE_SLEEPERS (expected 3)"

if [ -n "$ZOMBIES" ] && [ "$LIVE_SLEEPERS" -gt 0 ]; then
  echo "=> CONFIRMED: workers alive but wrappers are unreaped zombies (the bug)."
fi

section "keeping daemon alive for manual inspection (docker exec ...)"
tail -f /dev/null
