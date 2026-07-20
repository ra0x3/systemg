#!/usr/bin/env bash
# ABUSE: fire many `sysg restart` at ONE supervisor simultaneously.
#
# WHAT THIS ABUSES
#   Nobody restarts the same stack 12 times at once — but it is legal, and it
#   hammers the single-writer owner thread + config-swap path with concurrent
#   mutations. The supervisor must serialize them safely: survive, never crash,
#   never leave a service orphaned or duplicated, and end in a state where ps and
#   status agree. A race here would show up as a dead supervisor, a doubled PID,
#   a service left stopped, or a hang.
#
# HARD INVARIANTS (after the storm settles)
#   - the supervisor is still answering,
#   - each of the 3 services is running on exactly ONE pid,
#   - no orphaned `sleep` beyond the 3 expected,
#   - ps count == status running count == 3.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
N=12

section "boot the stack"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3
BASE_SLEEPS="$(pgrep -c -x sleep || echo 0)"
echo "baseline sleep procs: $BASE_SLEEPS"
[ "$BASE_SLEEPS" = "3" ]
check "$?" "exactly 3 service processes before the storm"

section "fire $N restarts simultaneously"
pids=""
i=0
while [ "$i" -lt "$N" ]; do
  sysg restart --config "$CONFIG" >"/tmp/r_$i.out" 2>"/tmp/r_$i.err" &
  pids="$pids $!"
  i=$((i+1))
done
FAILED=0
for p in $pids; do wait "$p" || FAILED=$((FAILED+1)); done
echo "$FAILED of $N restart invocations exited non-zero"
# Some concurrent restarts may lose a race and report a transient error; that is
# acceptable ONLY if the end state is consistent. We assert the end state below.
sleep 4

section "the supervisor SURVIVED the storm"
sysg status --config "$CONFIG" --format json >/tmp/st.json 2>/tmp/st.err
grep -qi "No running supervisor" /tmp/st.err && DEAD=1 || DEAD=0
[ "$DEAD" = "0" ]
check "$?" "supervisor still answering after $N concurrent restarts"

section "no orphaned or duplicated processes"
NOW_SLEEPS="$(pgrep -c -x sleep || echo 0)"
echo "sleep procs after storm: $NOW_SLEEPS (expected 3)"
[ "$NOW_SLEEPS" = "3" ]
check "$?" "exactly 3 service processes remain (no orphan, no duplicate)"

section "ps == status: every service running on exactly one pid"
S="$(cat /tmp/st.json)"
RUNNING=0
for svc in web api worker; do
  P="$(unit_field "$S" "$svc" pid)"
  ST="$(unit_field "$S" "$svc" state)"
  echo "  $svc -> pid:$P state:$ST"
  [ -n "$P" ] && [ "$P" != "absent" ] && pid_alive "$P" && [ "$ST" = "running" ] && RUNNING=$((RUNNING+1))
done
[ "$RUNNING" = "3" ]
check "$?" "all 3 services running on a live pid per status"
[ "$(unit_count "$S")" = "3" ]
check "$?" "status lists exactly 3 units (no duplicate rows)"

sysg stop --supervisor >/dev/null 2>&1
finish
