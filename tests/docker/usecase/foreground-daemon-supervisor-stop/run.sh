#!/usr/bin/env bash
# WORST-CASE: `stop --supervisor` while a foreground start holds the terminal.
#
# WHAT THIS ABUSES
#   A foreground `sysg start` is attached to the terminal, blocking on Ctrl-C. A
#   daemon project also runs. Then a DIFFERENT invocation shuts the whole
#   supervisor down (`sysg stop --supervisor`). The attached foreground must not
#   hang forever waiting on a supervisor that no longer exists — its process must
#   exit, and every service (foreground + daemon) must be torn down. No orphans.
#
# HARD INVARIANTS
#   - with both up, `stop --supervisor` exits without hanging,
#   - the attached foreground `sysg start` process EXITS (does not wedge),
#   - ZERO service processes survive (fg + daemon both reaped),
#   - a fresh `status` reports no running supervisor.
set -u
. /usecase/lib.sh

section "start daemon + foreground (foreground holds the terminal)"
sysg start --config /usecase/daemon.yaml --daemonize
check "$?" "daemon start exits 0"
sleep 2
# foreground start under a PTY, held 20s (long — we tear it down via supervisor stop)
python3 /usecase/fg_run.py /usecase/fg.yaml 20 >/tmp/fg.out 2>&1 &
FGJOB=$!
sleep 5
[ -n "$(pgrep -f FG_LINE)" ] && [ -n "$(pgrep -f DAEMON_LINE)" ]
check "$?" "both foreground and daemon services are running"

section "stop --supervisor while the foreground is attached"
timeout 20 sysg stop --supervisor >/tmp/sup.out 2>&1
RC=$?
cat /tmp/sup.out | grep -v WARN | head
[ "$RC" != "124" ]
check "$?" "stop --supervisor did not hang"

section "the attached foreground process exits (does not wedge)"
WAITED=0
while [ "$WAITED" -lt 12 ]; do
  if ! kill -0 "$FGJOB" 2>/dev/null; then break; fi
  sleep 1; WAITED=$((WAITED+1))
done
! kill -0 "$FGJOB" 2>/dev/null
check "$?" "the foreground start process exited after the supervisor stopped"
kill "$FGJOB" 2>/dev/null

section "everything is torn down — no orphans"
sleep 2
NFG="$(pgrep -f FG_LINE | wc -l | tr -d ' ')"
NDM="$(pgrep -f DAEMON_LINE | wc -l | tr -d ' ')"
echo "leftover fg=$NFG daemon=$NDM (both expect 0)"
[ "$NFG" = "0" ] && [ "$NDM" = "0" ]
check "$?" "no service processes survive the supervisor stop"
sysg status --config /usecase/daemon.yaml >/tmp/st.txt 2>&1
grep -qiE "SG0206|No running supervisor|OFFLINE" /tmp/st.txt
check "$?" "status reports no running supervisor"

finish
