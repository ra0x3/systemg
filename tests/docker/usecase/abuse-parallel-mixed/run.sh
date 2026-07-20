#!/usr/bin/env bash
# ABUSE: fire EVERY kind of mutation at one supervisor at the same instant.
#
# WHAT THIS ABUSES
#   Real callers do one thing at a time. Here we interleave restart, stop -s,
#   start -s, and status against the SAME live supervisor simultaneously, over
#   many rounds. Add / remove / read mutations all land on the single-writer
#   owner thread at once — the worst-case ordering. A race would surface as a
#   dead supervisor, a service wedged stopped, a doubled pid, an orphan, a hang,
#   or a status blob that disagrees with ps. The supervisor must serialize them,
#   survive, and converge to a state where every service is running once and
#   ps == status.
#
# HARD INVARIANTS (after the storm, once we restore all services)
#   - no invocation HANGS (each bounded by `timeout`),
#   - the supervisor is still answering,
#   - each of the 3 services runs on exactly ONE live pid,
#   - exactly 3 `sleep` processes (no orphan, no duplicate),
#   - status lists exactly 3 units.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
ROUNDS=6

section "boot the stack"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3
[ "$(pgrep -c -x sleep || echo 0)" = "3" ]
check "$?" "exactly 3 service processes before the storm"

section "$ROUNDS rounds of simultaneous restart / stop -s / start -s / status"
HANG=0
i=0
while [ "$i" -lt "$ROUNDS" ]; do
  timeout 20 sysg restart --config "$CONFIG" >/dev/null 2>&1 &
  P1=$!
  timeout 20 sysg stop --config "$CONFIG" -s api >/dev/null 2>&1 &
  P2=$!
  timeout 20 sysg start --config "$CONFIG" -s api >/dev/null 2>&1 &
  P3=$!
  timeout 20 sysg status --config "$CONFIG" --format json >/dev/null 2>&1 &
  P4=$!
  for p in "$P1" "$P2" "$P3" "$P4"; do
    wait "$p"; [ "$?" = "124" ] && HANG=$((HANG+1))
  done
  i=$((i+1))
done
echo "hung invocations: $HANG"
[ "$HANG" = "0" ]
check "$?" "no mixed invocation hung during the storm"

section "restore every service, then let it settle"
timeout 20 sysg restart --config "$CONFIG" >/tmp/restore.err 2>&1
RC=$?
cat /tmp/restore.err
[ "$RC" != "124" ]
check "$?" "restore restart did not hang"
sleep 4

section "the supervisor SURVIVED and answers"
S="$(sysg status --config "$CONFIG" --format json 2>/tmp/st.err)"
grep -qi "No running supervisor" /tmp/st.err && DEAD=1 || DEAD=0
[ "$DEAD" = "0" ]
check "$?" "supervisor still answering after the mixed storm"

section "ps == status: every service running on exactly one pid"
RUNNING=0
for svc in web api worker; do
  P="$(unit_field "$S" "$svc" pid demo)"
  ST="$(unit_field "$S" "$svc" state demo)"
  echo "  $svc -> pid:$P state:$ST"
  [ -n "$P" ] && [ "$P" != "absent" ] && pid_alive "$P" && [ "$ST" = "running" ] && RUNNING=$((RUNNING+1))
done
[ "$RUNNING" = "3" ]
check "$?" "all 3 services running on a live pid per status"
[ "$(unit_count "$S")" = "3" ]
check "$?" "status lists exactly 3 units (no duplicate/ghost rows)"
ORPHANS=0
for sp in $(pgrep -x sleep); do
  PP="$(ps -o ppid= -p "$sp" | tr -d ' ')"
  pid_alive "$PP" || ORPHANS=$((ORPHANS+1))
done
echo "reparented/orphaned sleeps (ppid dead): $ORPHANS (expected 0)"
[ "$ORPHANS" = "0" ]
check "$?" "no orphaned service process (every sleep has a live parent)"

sysg stop --supervisor >/dev/null 2>&1
finish
