#!/usr/bin/env bash
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot substantial history, then start a sparse current generation"
sysg start --config "$CONFIG" --daemonize >/dev/null
check "$?" "history generation starts"
sleep 2
: > /usecase/current
sysg restart --config "$CONFIG" -p bar -s foo >/dev/null
check "$?" "current generation starts"
sleep 1

section "the trivial snapshot is a fresh bounded tail"
sysg logs -p bar -s foo --no-follow --format json > /tmp/default.jsonl 2>/dev/null
COUNT="$(grep -Ec '"line":"(OLD|CURRENT)_' /tmp/default.jsonl)"
[ "$COUNT" = "100" ] && grep -q '"line":"CURRENT_C"' /tmp/default.jsonl
check "$?" "default snapshot returns 100 latest lines including current output"
! grep -q '"line":"OLD_001 ' /tmp/default.jsonl
check "$?" "default snapshot excludes the oldest history"

section "explicit limits and full history retain their distinct contracts"
sysg logs -p bar -s foo --no-follow -l 10 --format json > /tmp/ten.jsonl 2>/dev/null
TEN="$(grep -Ec '"line":"(OLD|CURRENT)_' /tmp/ten.jsonl)"
[ "$TEN" = "10" ] && grep -q '"line":"CURRENT_C"' /tmp/ten.jsonl
check "$?" "explicit line limit returns the latest requested lines"
sysg logs -p bar -s foo --no-follow --all --format json > /tmp/all.jsonl 2>/dev/null
ALL="$(grep -Ec '"line":"(OLD|CURRENT)_' /tmp/all.jsonl)"
[ "$ALL" = "153" ]
check "$?" "--all returns complete history through the current generation"

section "raw and filtered views keep the same latest-tail semantics"
sysg logs -p bar -s foo --no-follow -l 3 --raw > /tmp/raw.log 2>/dev/null
[ "$(wc -l < /tmp/raw.log)" = "3" ] && grep -qx 'CURRENT_C' /tmp/raw.log
check "$?" "raw output returns exactly the latest application lines"
sysg logs -p bar -s foo --no-follow --grep '^.*CURRENT_' --format json > /tmp/grep.jsonl 2>/dev/null
[ "$(wc -l < /tmp/grep.jsonl)" = "3" ] && grep -q '"line":"CURRENT_C"' /tmp/grep.jsonl
check "$?" "filtered output keeps every matching current line"

section "completed current lines are immediately durable"
LOG_PATH="$(sysg logs -p bar -s foo --path)"
grep -q 'CURRENT_C' "$LOG_PATH"
check "$?" "active log file agrees with the live snapshot"

section "the ordinary interactive command keeps every line at column zero"
python3 /usecase/pty_check.py follow sysg logs -p bar -l 1000 --follow > /tmp/project-follow.pty 2>/dev/null
check "$?" "project follow keeps complete line-framed backlog rows"
python3 /usecase/pty_check.py service sysg logs -p bar -s foo -l 1000 --follow > /tmp/service-follow.pty 2>/dev/null
check "$?" "service follow returns the complete requested tail with aligned rows"

section "stream refreshes clean frames even when log rows wrap"
python3 /usecase/pty_check.py stream sysg logs -p bar -s foo -l 3 --stream 1 > /tmp/stream.pty 2>/dev/null
check "$?" "interactive stream fully repaints wrapped frames"

sysg stop --supervisor >/dev/null 2>&1
finish
