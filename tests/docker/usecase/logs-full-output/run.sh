#!/usr/bin/env bash
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot a service that emits 200 numbered lines"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3

section "default logs returns the latest 100 lines"
FULL="$(sysg logs --config "$CONFIG" -p dev -s chatty --no-follow 2>/dev/null | grep -c '^.*LINE_[0-9]')"
echo "default logs LINE_ count: $FULL (expected 100)"
[ "$FULL" = "100" ]
check "$?" "default logs shows the latest 100 lines"

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

section "--all returns complete captured history"
ALL="$(sysg logs --config "$CONFIG" -p dev -s chatty --no-follow --all 2>/dev/null | grep -c '^.*LINE_[0-9]')"
echo "--all LINE_ count: $ALL (expected 200)"
[ "$ALL" = "200" ]
check "$?" "--all shows all 200 lines"

sysg stop --supervisor >/dev/null 2>&1
finish
