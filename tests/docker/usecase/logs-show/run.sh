#!/usr/bin/env bash
# USE CASE: `sysg logs` shows a service's captured output (snapshot mode).
#
# WHAT THIS TESTS
#   The default show mode. A running service emits numbered lines; `sysg logs -s`
#   must surface them (a one-shot snapshot for a non-interactive caller — no
#   hang). --format json must emit structured records too.
#
# HARD INVARIANTS
#   - `sysg logs -s chatty` prints captured LOG_LINE_ lines and exits (no hang),
#   - `sysg logs -s chatty --format json` emits JSON objects with the line.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot a chatty service, let it log"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 4

section "logs snapshot surfaces captured lines and exits"
timeout 15 sysg logs --config "$CONFIG" -s chatty --no-follow >/tmp/l.out 2>/tmp/l.err
RC=$?
[ "$RC" != "124" ]
check "$?" "logs snapshot did not hang"
cat /tmp/l.out | head
grep -q "LOG_LINE_" /tmp/l.out
check "$?" "captured LOG_LINE_ output is shown"

section "json format emits structured records"
timeout 15 sysg logs --config "$CONFIG" -s chatty --no-follow --format json >/tmp/j.out 2>&1
grep -q "LOG_LINE_" /tmp/j.out && grep -q "{" /tmp/j.out
check "$?" "--format json emits objects carrying the line"

sysg stop --supervisor >/dev/null 2>&1
finish
