#!/usr/bin/env bash
# USE CASE: `sysg logs` shows the FULL captured output by default, no silent cutoff.
#
# WHAT THIS TESTS (0.54.2 bug: logs randomly cut to a few lines)
#   A service emits 200 known numbered lines. Default `sysg logs` must return ALL
#   200 — no arbitrary truncation. A limit applies ONLY when -l/--lines is given.
#
# HARD INVARIANTS
#   - default `logs -p dev -s chatty` returns ALL 200 LINE_ lines,
#   - `logs ... -l 10` returns exactly 10 (the last 10),
#   - the count is deterministic (not a random 2-line cutoff).
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot a service that emits 200 numbered lines"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3

section "default logs returns ALL 200 lines (no silent cutoff)"
FULL="$(sysg logs --config "$CONFIG" -p dev -s chatty --no-follow 2>/dev/null | grep -c '^.*LINE_[0-9]')"
echo "default logs LINE_ count: $FULL (expected 200)"
[ "$FULL" = "200" ]
check "$?" "default logs shows all 200 lines"

section "explicit -l 10 limits to 10"
TEN="$(sysg logs --config "$CONFIG" -p dev -s chatty --no-follow -l 10 2>/dev/null | grep -c '^.*LINE_[0-9]')"
echo "-l 10 count: $TEN"
[ "$TEN" = "10" ]
check "$?" "-l 10 returns exactly 10 lines"

section "the last -l 10 lines are the LATEST (191..200), not a random slice"
LAST="$(sysg logs --config "$CONFIG" -p dev -s chatty --no-follow -l 10 2>/dev/null | grep -oE 'LINE_[0-9]+' | tail -1)"
echo "last line with -l 10: $LAST (expect LINE_200)"
[ "$LAST" = "LINE_200" ]
check "$?" "-l 10 returns the newest lines (tail), ending at LINE_200"

sysg stop --supervisor >/dev/null 2>&1
finish
