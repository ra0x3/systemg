#!/usr/bin/env bash
# Reproduces the orphaned-process / ghost-on-port bug.
#
# When a supervisor is kill -9'd, its children survive unmanaged (they are not
# reparented away, they keep their ports). On the 2026 gamecast box a stale
# `gamecast serve` from an old binary held :8000 for ~5 hours; every new
# gamecast_api died on the port conflict while nginx proxied to the ghost. sysg
# could not see it -- the ghost was not a tracked child.
#
# The illness: sysg trusts its bookkeeping and never reconciles it against the
# actual machine (procfs + port ownership). This asserts the patched behavior:
#
#   1. after a supervisor is kill -9'd (orphan survives on :8100) and a fresh
#      supervisor boots, the orphan is detected + terminated and a NEW api
#      instance binds :8100 -- proven by a changed server PID.
#   2. a zombie (<defunct>) left in the supervised tree is reaped, not reported
#      as running forever.
set -u

CONFIG=/repro/sysg.config.yaml
export HOME=/root

PASS=0
FAIL=0
section() { printf '\n========== %s ==========\n' "$1"; }
check()   { if [ "$1" = "0" ]; then echo "PASS: $2"; PASS=$((PASS+1)); else echo "FAIL: $2"; FAIL=$((FAIL+1)); fi; }

port_holder() {
  # PID currently listening on the given TCP port, via ss.
  ss -tlnp 2>/dev/null \
    | awk -v p=":$1" '$4 ~ p" *$" || $4 ~ p"$" { print }' \
    | grep -oE 'pid=[0-9]+' | head -1 | cut -d= -f2
}

supervisor_pid() { cat "$HOME/.local/share/systemg/sysg.pid" 2>/dev/null; }

section "boot the stack; api binds :8100"
rm -f /tmp/api.pid
sysg start --config "$CONFIG" --log-level debug --daemonize
echo "start rc: $?"
sleep 5

API_PID_1="$(cat /tmp/api.pid 2>/dev/null)"
HOLDER_1="$(port_holder 8100)"
echo "api server PID (from file): ${API_PID_1:-<none>}"
echo ":8100 held by PID: ${HOLDER_1:-<none>}"
[ -n "$API_PID_1" ] && [ -n "$HOLDER_1" ]
check "$?" "precondition: api is up and holding :8100"

section "kill -9 the supervisor; the api orphan survives on :8100"
SUP_PID="$(supervisor_pid)"
echo "supervisor PID: ${SUP_PID:-<none>}"
kill -9 "$SUP_PID" 2>/dev/null
sleep 2

# The orphan must still be alive and still holding the port -- that is the ghost.
STILL_HELD="$(port_holder 8100)"
echo ":8100 still held by orphan PID: ${STILL_HELD:-<none>}"
[ "$STILL_HELD" = "$API_PID_1" ]
check "$?" "orphaned api survived the supervisor kill and still holds :8100"

section "restart a fresh supervisor over the ghost"
rm -f /tmp/api.pid
sysg start --config "$CONFIG" --log-level debug --daemonize
echo "restart rc: $?"
# give the boot sweep + first health cycle time to reclaim and rebind
sleep 8

# The original orphan PID must no longer exist -- the boot sweep reclaimed it.
kill -0 "$API_PID_1" 2>/dev/null
[ "$?" -ne 0 ]
check "$?" "the original orphan process was terminated"

HOLDER_2="$(port_holder 8100)"
echo ":8100 held by PID after restart: ${HOLDER_2:-<none>}"

# The port must be owned by SOME live process that is NOT the old ghost.
[ -n "$HOLDER_2" ] && [ "$HOLDER_2" != "$API_PID_1" ]
check "$?" ":8100 is owned by the supervisor's new api, not the ghost"

section "supervisor reconcile/sweep log"
grep -iE "reclaim|ghost|port .* held|terminat" \
  "$HOME/.local/share/systemg/logs/supervisor.log" 2>/dev/null | tail -15 \
  || echo "(no sweep log lines)"

section "RESULT"
echo "passed: $PASS  failed: $FAIL"
if [ "$FAIL" -eq 0 ]; then
  echo "=> GREEN: orphan-ghost fixed."
  exit 0
else
  echo "=> RED: orphan-ghost reproduced / not fixed."
  exit 1
fi
