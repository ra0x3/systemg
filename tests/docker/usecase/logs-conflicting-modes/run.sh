#!/usr/bin/env bash
# USE CASE: mutually-exclusive logs modes are refused with SG0204.
#
# WHAT THIS TESTS
#   `logs` is five modes wearing one name; the flags that pick them are mutually
#   exclusive. Combining two (or --follow with a non-show mode) must be a typed
#   refusal, not undefined behavior.
#
# HARD INVARIANTS
#   - `logs --path --purge` exits non-zero with SG0204,
#   - `logs --prune --max-size 1MB --follow` exits non-zero with SG0204,
#   - a plain valid `logs` still works (guard against false positives).
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 2

section "--path --purge together is refused with SG0204"
sysg logs --config "$CONFIG" --path --purge >/tmp/a.out 2>/tmp/a.err
RC=$?
cat /tmp/a.out /tmp/a.err
[ "$RC" != "0" ]
check "$?" "conflicting modes exit non-zero"
grep -q "SG0204" /tmp/a.err
check "$?" "names SG0204"

section "--follow with a non-show mode is refused with SG0204"
sysg logs --config "$CONFIG" --prune --max-size 1MB --follow >/tmp/b.out 2>/tmp/b.err
RC=$?
cat /tmp/b.out /tmp/b.err
[ "$RC" != "0" ]
check "$?" "--follow with a mode exits non-zero"
grep -q "SG0204" /tmp/b.err
check "$?" "names SG0204"

section "a plain logs snapshot still works"
timeout 15 sysg logs --config "$CONFIG" -s chatty --no-follow >/tmp/c.out 2>&1
[ "$?" != "124" ]
check "$?" "valid logs snapshot is not falsely refused"

sysg stop --supervisor >/dev/null 2>&1
finish
