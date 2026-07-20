#!/usr/bin/env bash
# USE CASE: a foreground start survives its own boot window.
#
# WHAT THIS TESTS (real dogfooding bug)
#   `supervisor_running()` goes true the moment the CONTROL SOCKET appears —
#   well before any service is spawned. A foreground start then polled
#   `project_loaded_in_supervisor()`, which only answers yes once a unit is in
#   the status snapshot. During the boot window the honest answer is "not yet",
#   but both the follow thread and the attach wait-loop read it as "the project
#   was stopped elsewhere" and detached IMMEDIATELY.
#
#   The blast radius was larger than a missing log line: the detaching parent is
#   the process that OWNS THE TERMINAL. It exited, leaving the supervisor child
#   reparented to init. So on a slow-booting project you got a foreground start
#   that (a) streamed nothing ever, and (b) had no process left to receive
#   Ctrl-C — which then looked like a broken teardown rather than a dead parent.
#   Measured live on a real project: 6 bytes captured before, 1.6 MB after.
#
#   `warmup` sleeps 6s and `talker` depends on it, so no unit is registered for
#   several seconds — reproducing the window deterministically.
#
# EXPECTED OUTCOME
#   - the foreground parent is STILL ALIVE after the boot window;
#   - its terminal streamed the project's own marker line;
#   - a REAL terminal Ctrl-C (0x03 to the pty master, not `kill -INT`) then
#     tears the project down and the parent exits.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
OUT=/tmp/fg.out
PIDF=/tmp/fg.pid

section "foreground start of a SLOW-booting project"
python3 /usecase/fgpty.py "$CONFIG" "$OUT" "$PIDF" 120 &
sleep 20

FGPID="$(cat "$PIDF" 2>/dev/null)"
[ -n "$FGPID" ]
check "$?" "the foreground parent recorded a pid"

pid_alive "$FGPID"
check "$?" "the foreground parent SURVIVED the boot window (did not self-detach)"

BYTES="$(wc -c <"$OUT" 2>/dev/null | tr -d ' ')"
echo "captured bytes: $BYTES"
[ "${BYTES:-0}" -gt 100 ]
check "$?" "the terminal streamed real output (not an empty spinner line)"

grep -q FG_MARKER_LINE "$OUT"
check "$?" "the streamed output is THIS project's own service output"

section "a real terminal Ctrl-C tears it down"
# Count the talker's OWN pid from sysg, not by matching command strings: a
# `pgrep -f` pattern also matches this script's own shell line, which reports
# phantom survivors and makes a correct teardown look broken.
BEFORE="$(unit_field "$(sysg status --format json 2>/dev/null)" talker pid 2>/dev/null)"
echo "talker pid before: $BEFORE"
case "$BEFORE" in ''|absent|noparse|null|-) BEFORE="" ;; esac
[ -n "$BEFORE" ]
check "$?" "captured the talker's real pid before Ctrl-C"

: > "$PIDF.ctl"
sleep 10

pid_alive "$FGPID"
[ "$?" != "0" ]
check "$?" "the foreground parent exited on Ctrl-C"

! pid_alive "$BEFORE"
check "$?" "Ctrl-C stopped the project's services (pid $BEFORE is gone)"

sysg stop --supervisor >/dev/null 2>&1
finish
