#!/bin/bash
export PATH="$HOME/.local/bin:$HOME/.sysg/bin:$PATH"
MODE="$1"; mkdir -p /ready; rm -f /ready/*
now() { date +%s.%N; }
el() { awk -v a="$T0" -v b="$(now)" 'BEGIN{printf "%.2f", b-a}'; }
case "$MODE" in
sysg)
  printf 'version: "2"\nlogs:\n  sink: none\nservices:\n  w:\n    command: "/h/slow.sh"\n    deployment:\n      health_check:\n        command: "test -f /ready/w"\n        interval: "1s"\n' > /tmp/s.yaml
  T0=$(now)
  ( sysg start -c /tmp/s.yaml --daemonize >/dev/null 2>&1 ) &
  UP=""
  for i in $(seq 1 300); do
    j=$(sysg status -c /tmp/s.yaml --format json 2>/dev/null)
    echo "$j" | grep -qiE '"(state|status)"[[:space:]]*:[[:space:]]*"(running|healthy)"' && { UP=$(el); break; }
    sleep 0.1
  done ;;
supervisor)
  { echo '[unix_http_server]'; echo 'file=/tmp/sd.sock'; echo '[supervisord]'; echo 'logfile=/tmp/sd.log'
    echo '[rpcinterface:supervisor]'
    echo 'supervisor.rpcinterface_factory = supervisor.rpcinterface:make_main_rpcinterface'
    echo '[supervisorctl]'; echo 'serverurl=unix:///tmp/sd.sock'
    echo '[program:w]'; echo 'command=/h/slow.sh'; echo 'autostart=true'; } > /tmp/sd.conf
  T0=$(now); supervisord -c /tmp/sd.conf >/dev/null 2>&1
  UP=""
  for i in $(seq 1 300); do
    supervisorctl -c /tmp/sd.conf status w 2>/dev/null | grep -q RUNNING && { UP=$(el); break; }
    sleep 0.1
  done ;;
esac
READY=""
for i in $(seq 1 300); do [ -f /ready/w ] && { READY=$(el); break; }; sleep 0.1; done
echo "RESULT,$MODE,reported_up=${UP}s,actually_ready=${READY}s,lie_window=$(awk -v u=$UP -v r=$READY 'BEGIN{printf "%.2f", r-u}')s"
