#!/usr/bin/env bash
# USE CASE: a port collision reports SG0105, not a bare OS error.
#
# WHAT THIS TESTS (real dogfooding bug)
#   When a service cannot bind because its port is taken, sysg used to surface a
#   raw "Address already in use (os error 48)" buried in a generic immediate-exit
#   (SG0102) / restart-loop log. That tells the user nothing actionable, and it is
#   the single most common start failure. It must be its own typed code (SG0105)
#   that names the problem — and the port when the output reveals it.
#
# EXPECTED OUTCOME
#   - With port 8080 pre-bound by another process, starting `web` (which wants
#     8080) fails with SG0105 on stderr, naming the port-in-use condition.
#   - The message is sysg-native (no lsof/kill/netstat guidance).
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "occupy port 8080 before sysg starts its service"
python3 -m http.server 8080 --bind 127.0.0.1 >/dev/null 2>&1 &
SQUATTER=$!
sleep 2
kill -0 "$SQUATTER" 2>/dev/null
check "$?" "a squatter process is holding port 8080"

section "starting the service on the taken port reports SG0105"
sysg start --config "$CONFIG" --daemonize 2>/tmp/start.err
RC=$?
echo "start rc: $RC"
echo "--- stderr ---"; cat /tmp/start.err
[ "$RC" != "0" ]
check "$?" "start exits non-zero when the port is taken"
stderr_has_code SG0105 /tmp/start.err
check "$?" "stderr names SG0105 (port in use), not a bare OS error"
grep -qiE 'already in use|could not bind' /tmp/start.err
check "$?" "the message says the port is already in use"

section "the guidance is sysg-native"
! grep -qiE 'lsof|netstat|kill -9' /tmp/start.err
check "$?" "no external-tool guidance (lsof/netstat/kill) in the help"

kill "$SQUATTER" 2>/dev/null
sysg stop --supervisor >/dev/null 2>&1
finish
