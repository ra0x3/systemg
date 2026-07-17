#!/usr/bin/env bash
# USE CASE (HAPPY): re-submitting with -c clears the dirty state and applies.
#
# WHAT THIS TESTS
#   The other half of the cache model: passing -c IS submitting the latest
#   manifest, so it is never refused; it applies the change AND refreshes the
#   cache, after which bare commands work again. Also: a bare command against an
#   UNCHANGED manifest is never falsely refused.
#
# HARD INVARIANTS
#   - unchanged manifest -> bare status proceeds (no SG0018),
#   - dirty the manifest -> bare restart refused (SG0018),
#   - restart WITH -c succeeds AND applies the new GREETING,
#   - after the -c resubmit, bare status proceeds again (cache refreshed).
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot with -c, then a bare status on the UNCHANGED manifest is fine"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 2
sysg status >/tmp/s.out 2>/tmp/s.err
RC=$?
! grep -q "SG0018" /tmp/s.err
check "$?" "bare status is NOT refused when the manifest is unchanged"

section "dirty the manifest -> bare restart is refused with SG0018"
python3 -c "p='$CONFIG'; s=open(p).read().replace('hello','howdy'); open(p,'w').write(s)"
sysg restart >/tmp/r.out 2>/tmp/r.err
RC=$?
[ "$RC" != "0" ] && grep -q "SG0018" /tmp/r.err
check "$?" "bare restart refused with SG0018 while dirty"

section "restart WITH -c succeeds and APPLIES the change"
sysg restart --config "$CONFIG" >/tmp/rc.out 2>/tmp/rc.err
RC=$?
cat /tmp/rc.err | grep -v WARN | head
[ "$RC" = "0" ]
check "$?" "restart -c exits 0 (submitting the latest manifest is never refused)"
sleep 2
# the service re-echoes GREETING on boot; the new value must be live
sysg logs --config "$CONFIG" -s web --no-follow 2>/dev/null | grep -q "GREETING_IS_howdy"
check "$?" "the new GREETING (howdy) is live after the -c resubmit"

section "after the resubmit, bare commands work again (cache refreshed)"
sysg status >/tmp/s2.out 2>/tmp/s2.err
! grep -q "SG0018" /tmp/s2.err
check "$?" "bare status is no longer refused (dirty state cleared)"

sysg stop --supervisor >/dev/null 2>&1
finish
