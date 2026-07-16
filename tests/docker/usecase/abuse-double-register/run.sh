#!/usr/bin/env bash
# ABUSE: register the SAME project concurrently and repeatedly.
#
# WHAT THIS ABUSES
#   `start -c <same-file>` fired many times at once, plus a second file that
#   declares the SAME project id `demo`. A user would never do this, but it is
#   legal and it races the project-registration path: two threads both trying to
#   insert/boot project `demo`. The supervisor must be idempotent — one
#   registration, one set of processes — never double-boot `demo` into two live
#   copies of each service, never leak PIDs across the duplicate registrations.
#
# HARD INVARIANTS
#   - after the concurrent registrations, demo has exactly 2 services running,
#   - exactly 2 `sleep` processes total (NOT 4, 6, ... from double-boot),
#   - status lists exactly 2 units (no duplicate/ghost rows),
#   - the supervisor is alive.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
DUP=/usecase/dup.yaml
N=8

section "prepare a second file declaring the SAME project id 'demo'"
cp "$CONFIG" "$DUP"
check "$?" "duplicate-config file created"

section "boot demo, then fire $N concurrent re-registrations (same + dup file)"
sysg start --config "$CONFIG" --daemonize
check "$?" "initial start exits 0"
sleep 2
pids=""
i=0
while [ "$i" -lt "$N" ]; do
  if [ $((i % 2)) -eq 0 ]; then F="$CONFIG"; else F="$DUP"; fi
  timeout 20 sysg start --config "$F" --daemonize >/dev/null 2>&1 &
  pids="$pids $!"
  i=$((i+1))
done
HANG=0
for p in $pids; do wait "$p"; [ "$?" = "124" ] && HANG=$((HANG+1)); done
echo "hung: $HANG"
[ "$HANG" = "0" ]
check "$?" "no concurrent registration hung"
sleep 4

section "demo was NOT double-booted"
NOW_SLEEPS="$(pgrep -c -x sleep || echo 0)"
echo "sleep procs: $NOW_SLEEPS (expected 2)"
[ "$NOW_SLEEPS" = "2" ]
check "$?" "exactly 2 service processes (no double-boot into 4/6/...)"

section "status is consistent: 2 units, both running, supervisor alive"
S="$(sysg status --config "$CONFIG" --format json 2>/tmp/st.err)"
grep -qi "No running supervisor" /tmp/st.err && DEAD=1 || DEAD=0
[ "$DEAD" = "0" ]
check "$?" "supervisor alive after the registration storm"
[ "$(unit_count "$S")" = "2" ]
check "$?" "status lists exactly 2 units (no duplicate rows)"
RUN=0
for svc in web api; do
  P="$(unit_field "$S" "$svc" pid demo)"
  [ -n "$P" ] && [ "$P" != "absent" ] && pid_alive "$P" && RUN=$((RUN+1))
done
[ "$RUN" = "2" ]
check "$?" "both demo services running on live pids"

sysg stop --supervisor >/dev/null 2>&1
finish
