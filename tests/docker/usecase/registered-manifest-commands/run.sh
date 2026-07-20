#!/usr/bin/env bash
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot and register the manifest"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 2
PID_BEFORE="$(pgrep -f 'GREETING_IS' | head -1)"
[ -n "$PID_BEFORE" ]
check "$?" "web is running before the manifest changes"

section "edit the registered manifest"
python3 -c "p='$CONFIG'; s=open(p).read().replace('hello','howdy'); open(p,'w').write(s)"
check "$?" "manifest edited on disk"

section "bare read commands use the registered project"
for cmd in "status" "inspect -s web" "logs -s web --no-follow"; do
  sysg $cmd >/tmp/c.out 2>/tmp/c.err
  RC=$?
  [ "$RC" = "0" ]
  check "$?" "bare '$cmd' succeeds"
done

PID_AFTER_READS="$(pgrep -f 'GREETING_IS' | head -1)"
[ "$PID_AFTER_READS" = "$PID_BEFORE" ] && pid_alive "$PID_AFTER_READS"
check "$?" "read commands leave web on its original pid"

section "bare control commands use the registered project"
sysg stop -s web >/tmp/stop.out 2>/tmp/stop.err
check "$?" "bare stop targets web"
if pid_alive "$PID_BEFORE"; then
  check 1 "stop left the original web process alive"
else
  check 0 "stop terminated the original web process"
fi

sysg start -s web >/tmp/start.out 2>/tmp/start.err
check "$?" "bare start targets web"
sleep 2
S="$(sysg status --format json 2>/dev/null)"
PID_AFTER_START="$(unit_field "$S" web pid demo)"
[ -n "$PID_AFTER_START" ] && [ "$PID_AFTER_START" != "$PID_BEFORE" ] && pid_alive "$PID_AFTER_START"
check "$?" "web restarts on a new live pid"

sysg stop --supervisor >/dev/null 2>&1
finish
