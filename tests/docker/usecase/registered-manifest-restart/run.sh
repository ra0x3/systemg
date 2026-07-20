#!/usr/bin/env bash
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot and register the manifest"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 2
S1="$(sysg status --format json 2>/dev/null)"
PID_BEFORE="$(unit_field "$S1" web pid demo)"
[ -n "$PID_BEFORE" ] && pid_alive "$PID_BEFORE"
check "$?" "web is running before the manifest changes"

section "edit the registered manifest"
python3 -c "p='$CONFIG'; s=open(p).read().replace('hello','howdy'); open(p,'w').write(s)"
check "$?" "manifest edited on disk"

section "bare restart adopts the registered manifest"
sysg restart >/tmp/r.out 2>/tmp/r.err
RC=$?
grep -v WARN /tmp/r.err | head
[ "$RC" = "0" ]
check "$?" "bare restart exits 0"
sleep 2

sysg logs -s web --no-follow 2>/dev/null | grep -q "GREETING_IS_howdy"
check "$?" "bare restart applies the edited manifest"

S2="$(sysg status --format json 2>/dev/null)"
PID_AFTER="$(unit_field "$S2" web pid demo)"
[ -n "$PID_AFTER" ] && [ "$PID_AFTER" != "$PID_BEFORE" ] && pid_alive "$PID_AFTER"
check "$?" "web runs on a new live pid"

sysg stop --supervisor >/dev/null 2>&1
finish
