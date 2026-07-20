#!/usr/bin/env bash
# USE CASE: a foreground project and a daemonized project coexist cleanly.
#
# WHAT THIS TESTS (user prod bug, 0.54.2)
#   Run `sysg start` (foreground, project A) while a separate `sysg start
#   --daemonize` (project B) is running. Three things must hold:
#     1. `sysg logs -p A` shows ONLY A's lines, `-p B` shows ONLY B's (no bleed),
#     2. Ctrl-C on the foreground REAPS A's service processes (no orphan holding
#        a port — the node/yarn leak the user hit),
#     3. the daemonized B SURVIVES the foreground stop (not orphaned/killed).
#
# HARD INVARIANTS
#   - while both run: FG_LINE and DAEMON_LINE procs both alive; logs isolated,
#   - after foreground Ctrl-C: ZERO FG_LINE procs (reaped, no orphan),
#   - after foreground Ctrl-C: DAEMON_LINE procs STILL alive + supervisor answers.
set -u
. /usecase/lib.sh

section "start the daemon project, then the foreground project"
sysg start --config /usecase/daemon.yaml --daemonize
check "$?" "daemon start exits 0"
sleep 2
DPID="$(pgrep -f DAEMON_LINE | head -1)"
echo "daemon svc pid: $DPID"
[ -n "$DPID" ]
check "$?" "daemon service is running"

# foreground start under a PTY, held 8s then Ctrl-C'd (backgrounded so we can probe)
python3 /usecase/fg_run.py /usecase/fg.yaml 8 &
FGJOB=$!
sleep 5

section "both projects run; logs are isolated (no bleed)"
[ -n "$(pgrep -f FG_LINE | head -1)" ]
check "$?" "foreground service is running alongside the daemon"
A="$(sysg logs --config /usecase/fg.yaml -p fgproj --no-follow 2>/dev/null)"
echo "$A" | grep -q "FG_LINE" && ! echo "$A" | grep -q "DAEMON_LINE"
check "$?" "logs -p fgproj shows ONLY FG_LINE (no daemon bleed)"
B="$(sysg logs --config /usecase/daemon.yaml -p dmproj --no-follow 2>/dev/null)"
echo "$B" | grep -q "DAEMON_LINE" && ! echo "$B" | grep -q "FG_LINE"
check "$?" "logs -p dmproj shows ONLY DAEMON_LINE"

section "wait for the foreground Ctrl-C to complete"
wait "$FGJOB" 2>/dev/null
sleep 3

section "the foreground service is REAPED (no orphan holding resources)"
FGLEFT="$(pgrep -c -x sleep 2>/dev/null; pgrep -f FG_LINE | wc -l | tr -d ' ')"
NFG="$(pgrep -f FG_LINE | wc -l | tr -d ' ')"
echo "leftover FG_LINE procs: $NFG (expect 0)"
[ "$NFG" = "0" ]
check "$?" "foreground service processes are gone after Ctrl-C (no orphan)"

section "the daemonized project SURVIVES the foreground stop"
NDM="$(pgrep -f DAEMON_LINE | wc -l | tr -d ' ')"
echo "daemon procs after fg stop: $NDM (expect >= 1)"
[ "$NDM" -ge 1 ]
check "$?" "daemon service still running (not orphaned by foreground stop)"
DPID_AFTER="$(pgrep -f DAEMON_LINE | head -1)"
[ "$DPID_AFTER" = "$DPID" ]
check "$?" "daemon kept its original pid (never bounced)"
S="$(sysg status --config /usecase/daemon.yaml --format json 2>/tmp/st.err)"
grep -qi "No running supervisor" /tmp/st.err && DEAD=1 || DEAD=0
[ "$DEAD" = "0" ]
check "$?" "supervisor still answering after the foreground stop"

sysg stop --supervisor >/dev/null 2>&1
finish
