#!/usr/bin/env bash
# USE CASE: two DIFFERENT projects run in the foreground at the same time.
#
# WHAT THIS TESTS
#   The 0.54.2 limitation the refactor lifted: you could not run
#   `sysg -c foo.yaml` in term1 AND `sysg -c bar.yaml` in term2 concurrently.
#   Each foreground start attaches its project to the shared resident supervisor,
#   so both projects' services must run at once and status must list both.
#
# HARD INVARIANTS
#   - foreground-start foo, then (while it holds the terminal) foreground-start
#     bar in a second terminal,
#   - BOTH foosvc and barsvc are running,
#   - status lists both projects' services.
set -u
. /usecase/lib.sh

section "term1: foreground-start project foo"
python3 /usecase/fg2.py /usecase/foo.yaml 12 &
T1=$!
sleep 4
pgrep -f 'foosvc\|sleep 3000' >/dev/null
# foo has one service; confirm a sleep is up
[ "$(pgrep -x sleep | wc -l | tr -d ' ')" -ge 1 ]
check "$?" "foo's service is running in the foreground"

section "term2: foreground-start project bar WHILE foo holds term1"
python3 /usecase/fg2.py /usecase/bar.yaml 8 &
T2=$!
sleep 4

section "BOTH projects' services run concurrently"
NOW="$(pgrep -x sleep | wc -l | tr -d ' ')"
echo "sleep procs (both projects): $NOW (expect >= 2)"
[ "$NOW" -ge 2 ]
check "$?" "both foo and bar services run at the same time"

section "status lists both projects"
S="$(sysg status --config /usecase/foo.yaml --format json 2>/dev/null)"
FOO="$(unit_field "$S" foosvc pid foo)"
BAR="$(unit_field "$S" barsvc pid bar)"
echo "foosvc pid: $FOO   barsvc pid: $BAR"
[ -n "$FOO" ] && [ "$FOO" != "absent" ] && pid_alive "$FOO"
check "$?" "status shows foo's service running"
[ -n "$BAR" ] && [ "$BAR" != "absent" ] && pid_alive "$BAR"
check "$?" "status shows bar's service running (second foreground project)"

# reap the foreground holders + supervisor
kill "$T1" "$T2" 2>/dev/null
sysg stop --supervisor >/dev/null 2>&1
sysg purge --force >/dev/null 2>&1
finish
