#!/usr/bin/env bash
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
LOG="$HOME/.local/share/systemg/logs/supervisor.log"
PROXY=18090

load_start() {
  rm -f "$2" "$2.ready" "$2.stop"
  python3 /usecase/loadgen.py "$1" "$2" &
  LOADGEN=$!
  for _ in $(seq 1 100); do
    [ -f "$2.ready" ] && return 0
    sleep 0.1
  done
  return 1
}

load_stop() {
  touch "$1.stop"
  wait "$LOADGEN"
}

proxy_listening() {
  python3 -c 'import socket, sys; socket.create_connection(("127.0.0.1", int(sys.argv[1])), 1).close()' "$PROXY" 2>/dev/null
}

echo "server 127.0.0.1:18082;" > /tmp/upstream.conf
echo 18082 > /tmp/active-slot

nginx -c /usecase/nginx.conf &
NGINX=$!
trap 'kill "$NGINX" 2>/dev/null; wait "$NGINX" 2>/dev/null' EXIT
for _ in $(seq 1 50); do
  proxy_listening && break
  sleep 0.1
done
proxy_listening
check "$?" "nginx accepts connections on $PROXY"

sysg start -c "$CONFIG" --daemonize
check "$?" "blue/green web and fixed-port control start"

section "negative control: fixed-port restart under load"
load_start 18084 /tmp/control.json
check "$?" "load reaches the control service"
sysg restart -p demo -s control >/tmp/control.out 2>/tmp/control.err
check "$?" "control restart succeeds"
load_stop /tmp/control.json
check "$?" "control load generator stops cleanly"
CONTROL_FAILURES="$(python3 /usecase/analyze.py failures /tmp/control.json 2>/dev/null)"
[ "$CONTROL_FAILURES" -gt 0 ]
check "$?" "load sees the outage a fixed-port restart causes ($CONTROL_FAILURES failed requests)"
grep -q "Service 'control' uses configured port 18084; switching to immediate restart semantics" "$LOG"
check "$?" "control took the immediate fallback"

section "blue/green restarts under load"
load_start "$PROXY" /tmp/web.json
check "$?" "load reaches web through the proxy"
: > /tmp/windows
for flip in 1 2 3 4 5; do
  read -r OLD_PID OLD_PORT <<< "$(http_get "http://127.0.0.1:$PROXY/")"
  START="$(date +%s.%N)"
  sysg restart -p demo -s web >"/tmp/flip-$flip.out" 2>"/tmp/flip-$flip.err"
  RC=$?
  END="$(date +%s.%N)"
  read -r NEW_PID NEW_PORT <<< "$(http_get "http://127.0.0.1:$PROXY/")"
  check "$RC" "flip $flip: restart exits 0"
  [ "$NEW_PID" != "$OLD_PID" ] && [ "$NEW_PORT" != "$OLD_PORT" ] \
    && [ "$(cat /tmp/active-slot)" = "$NEW_PORT" ]
  check "$?" "flip $flip: proxy moved from slot $OLD_PORT to $NEW_PORT"
  ! pid_alive "$OLD_PID"
  check "$?" "flip $flip: old generation $OLD_PID is gone"
  [ "$(cat "/tmp/pinned-$flip" 2>/dev/null)" = "200 $OLD_PID" ]
  check "$?" "flip $flip: request in flight on the old generation at reload finished"
  echo "$flip $START $END $OLD_PID $NEW_PID" >> /tmp/windows
done
load_stop /tmp/web.json
check "$?" "web load generator stops cleanly"

while read -r flip start end old new; do
  python3 /usecase/analyze.py window /tmp/web.json "$start" "$end" "$old" "$new"
  check "$?" "flip $flip: traffic went from $old to $new during the restart"
done < /tmp/windows

WEB_FAILURES="$(python3 /usecase/analyze.py failures /tmp/web.json)"
[ "$WEB_FAILURES" = "0" ]
check "$?" "zero failed requests across five blue/green restarts"
python3 /usecase/analyze.py latency /tmp/web.json 5
check "$?" "no request took 5s or longer"
grep -q "Performing blue/green rolling restart for service: web" "$LOG"
check "$?" "web took the blue/green path"

sysg stop --supervisor >/dev/null 2>&1
finish
