#!/bin/bash
export PATH="$HOME/.local/bin:$HOME/.sysg/bin:$PATH"
MODE="$1"; mkdir -p /ready; rm -f /ready/*
count() { pgrep -c -f "sleep 3600" 2>/dev/null || echo 0; }
case "$MODE" in
sysg)
  printf 'version: "2"\nlogs:\n  sink: none\nservices:\n  w:\n    command: "/h/worker.sh"\n' > /tmp/s.yaml
  sysg start -c /tmp/s.yaml --daemonize >/dev/null 2>&1
  for i in $(seq 1 100); do [ -f /ready/w ] && break; sleep 0.1; done; sleep 1
  BEFORE=$(count)
  sysg stop -c /tmp/s.yaml >/dev/null 2>&1
  ;;
supervisor)
  { echo '[unix_http_server]'; echo 'file=/tmp/sd.sock'; echo '[supervisord]'
    echo 'logfile=/tmp/sd.log'; echo 'pidfile=/tmp/sd.pid'
    echo '[rpcinterface:supervisor]'
    echo 'supervisor.rpcinterface_factory = supervisor.rpcinterface:make_main_rpcinterface'
    echo '[supervisorctl]'; echo 'serverurl=unix:///tmp/sd.sock'
    echo '[program:w]'; echo 'command=/h/worker.sh'; echo 'autostart=true'
    [ "$2" = "group" ] && { echo 'stopasgroup=true'; echo 'killasgroup=true'; }; } > /tmp/sd.conf
  supervisord -c /tmp/sd.conf >/dev/null 2>&1
  for i in $(seq 1 100); do [ -f /ready/w ] && break; sleep 0.1; done; sleep 1
  BEFORE=$(count)
  supervisorctl -c /tmp/sd.conf stop w >/dev/null 2>&1
  ;;
esac
sleep 3
AFTER=$(count)
echo "ORPHAN,$MODE,before=$BEFORE,after=$AFTER,leaked=$AFTER"
ps -eo pid,ppid,comm,args | grep "sleep 3600" | grep -v grep | head -5
