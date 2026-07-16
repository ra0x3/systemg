#!/usr/bin/env bash
# USE CASE: `logs --purge` clears logs so the READER stops showing them.
#
# WHAT THIS TESTS
#   The real contract, and the disk≠reader bug it fixes: the supervisor serves
#   `sysg logs` from an in-memory live buffer, not the files. Purging files
#   CLI-side left the reader replaying "purged" lines. `logs --purge` now routes
#   through the supervisor so BOTH the files and the live buffer are dropped.
#
# HARD INVARIANTS
#   - before purge, `sysg logs -s chatty` shows captured LOG_LINE_ content,
#   - `logs --purge` exits 0,
#   - AFTER purge, `sysg logs -s chatty` shows NONE of it — the reader agrees
#     with the wipe (this is what regressed before the ClearLogs fix).
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot a chatty service and let it log"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 5

section "reader shows captured content before purge"
timeout 15 sysg logs --config "$CONFIG" -s chatty --no-follow >/tmp/before.out 2>/dev/null
BEFORE="$(grep -c LOG_LINE_ /tmp/before.out)"
echo "reader LOG_LINE_ count before purge: $BEFORE"
[ "$BEFORE" -gt 0 ]
check "$?" "reader shows LOG_LINE_ content before purge"

section "stop the service so nothing re-emits, then purge"
sysg stop --config "$CONFIG" -s chatty >/dev/null 2>&1
sleep 2
sysg logs --config "$CONFIG" --purge >/tmp/pg.out 2>/tmp/pg.err
RC=$?
cat /tmp/pg.out /tmp/pg.err
[ "$RC" = "0" ]
check "$?" "logs --purge exits 0"

section "reader shows NOTHING after purge (disk == reader)"
sleep 1
timeout 15 sysg logs --config "$CONFIG" -s chatty --no-follow >/tmp/after.out 2>/dev/null
AFTER="$(grep -c LOG_LINE_ /tmp/after.out)"
echo "reader LOG_LINE_ count after purge: $AFTER (expected 0)"
[ "$AFTER" = "0" ]
check "$?" "reader no longer serves purged content"

sysg stop --supervisor >/dev/null 2>&1
finish
