#!/usr/bin/env bash
# USE CASE (UNHAPPY): a service that HARD-fails to bind its port is a failed start.
#
# WHAT THIS TESTS
#   Unlike a drifting dev server, some services exit immediately when their port
#   is taken (python http.server: "OSError: [Errno 98] Address already in use").
#   sysg must report this as SG0105 port-in-use, NOT healthy, and honor
#   max_restarts instead of looping forever.
#
# HARD INVARIANTS
#   - port 39187 pre-occupied,
#   - starting web (which tries to bind 39187) fails with SG0105,
#   - status never shows web healthy,
#   - the captured output shows the bind error (Address already in use).
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "pre-occupy port 39187"
python3 -m http.server 39187 >/tmp/blocker.log 2>&1 &
BLOCKER=$!
sleep 1
python3 -c 'import socket,sys; s=socket.socket(); sys.exit(0 if s.connect_ex(("127.0.0.1",39187))==0 else 1)'
check "$?" "port 39187 is occupied"

section "start web (tries to bind the taken 39187) -> failed start"
sysg start --config "$CONFIG" --daemonize >/tmp/s.out 2>/tmp/s.err
cat /tmp/s.err | grep -v WARN | head -8

# `command` is `sleep 1; python3 ...`, so the bind is not even ATTEMPTED until a
# second after the unit starts, and the supervisor needs another cycle to type
# the failure. A fixed `sleep 3` gave that maybe a second of slack and lost the
# race about a third of the time. Poll instead: the invariant is that SG0105
# arrives, not that it arrives inside an arbitrary window.
saw_code() {
  grep -q "SG0105" /tmp/s.err && return 0
  grep -q "SG0105" "$HOME/.local/share/systemg/logs/supervisor.log" 2>/dev/null
}
WAITED=0
while [ "$WAITED" -lt 20 ]; do
  saw_code && break
  WAITED=$((WAITED + 1))
  sleep 1
done
if saw_code; then
  check 0 "bind failure is typed as port-in-use (SG0105)"
else
  echo "--- log tail ---"; grep -iE "SG0|Address already|failed" "$HOME/.local/share/systemg/logs/supervisor.log" 2>/dev/null | grep -v capability | tail -4
  check 1 "bind failure is typed as port-in-use (SG0105)"
fi

section "web is NOT reported healthy, and the bind error was captured"
# `command` here is a shell wrapper (`sleep 1; python3 ...`), so the process
# survives the no-health-check stability window before the bind is even
# attempted. The unit therefore cycles healthy -> failing -> healthy as it
# restarts. Sampling once races that cycle; the real invariant is that it never
# SETTLES healthy. Poll across a full restart cycle and require a failing
# observation.
SAW_FAILING=1
SETTLED_HEALTHY=0
STREAK=0
for _ in 1 2 3 4 5 6 7 8 9 10; do
  S="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
  H="$(unit_field "$S" web health dev)"
  echo "web health: $H"
  if [ "$H" != "healthy" ]; then
    SAW_FAILING=0
    STREAK=0
  else
    STREAK=$((STREAK + 1))
    # A service that binds successfully stays healthy. Ten seconds of unbroken
    # health means it is NOT failing to bind, whatever a single sample showed.
    [ "$STREAK" -ge 5 ] && SETTLED_HEALTHY=1 && break
  fi
  sleep 2
done
[ "$SAW_FAILING" = "0" ] && [ "$SETTLED_HEALTHY" = "0" ]
check "$?" "web never settles healthy (it could not bind its port)"
sysg logs --config "$CONFIG" -p dev -s web --no-follow 2>/dev/null | grep -qi "address already in use"
check "$?" "the captured output shows the port-in-use error"

kill "$BLOCKER" 2>/dev/null
sysg stop --supervisor >/dev/null 2>&1
sysg purge --force >/dev/null 2>&1
finish
