#!/usr/bin/env bash
# USE CASE: status does not hang on a frozen supervisor; it reports degraded.
#
# WHAT THIS TESTS
#   Status used to call an UNTIMED send_command, so a supervisor whose process is
#   alive but not answering (frozen / wedged / mid-shutdown) made `sysg status`
#   hang forever. Now the health probe bounds the wait: a not-answering daemon is
#   reported as SG0205 (not responding), the reading is shown from disk, and the
#   command RETURNS instead of hanging.
#
# EXPECTED OUTCOME
#   - Boot demo; SIGSTOP the supervisor (alive pid, dead socket).
#   - `sysg status` returns within seconds (no hang), exits non-zero, and names
#     SG0205 on stderr.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
STATE_DIR="$HOME/.local/share/systemg"

section "boot the config"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3
SUP="$(cat "$STATE_DIR/sysg.pid")"
[ -n "$SUP" ] && pid_alive "$SUP"
check "$?" "supervisor process alive"

section "freeze the supervisor (alive pid, dead socket)"
kill -STOP "$SUP"
pid_alive "$SUP"
check "$?" "frozen supervisor pid STILL alive"

section "status returns degraded (SG0205) instead of hanging"
timeout 20 sysg status --config "$CONFIG" >/tmp/out.txt 2>/tmp/err.txt
RC=$?
echo "status rc=$RC"
echo "--- stderr ---"; cat /tmp/err.txt
[ "$RC" != "124" ]
check "$?" "status did NOT hang (no timeout)"
[ "$RC" != "0" ]
check "$?" "status exits non-zero while the supervisor is wedged"
stderr_has_code SG0205 /tmp/err.txt
check "$?" "stderr names SG0205 (supervisor not responding)"

kill -CONT "$SUP" 2>/dev/null
kill -9 "$SUP" 2>/dev/null
pkill -9 sleep 2>/dev/null || true
finish
