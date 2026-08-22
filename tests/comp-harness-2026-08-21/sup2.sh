#!/bin/bash
N="$1"; mkdir -p /ready; rm -f /ready/*
pss(){ awk '/^Pss:/{s+=$2} END{print s+0}' /proc/$1/smaps_rollup 2>/dev/null; }
{ echo '[unix_http_server]'; echo 'file=/tmp/sd.sock'; echo '[supervisord]'
  echo 'logfile=/tmp/sd.log'; echo 'pidfile=/tmp/sd.pid'
  echo '[rpcinterface:supervisor]'
  echo 'supervisor.rpcinterface_factory = supervisor.rpcinterface:make_main_rpcinterface'
  echo '[supervisorctl]'; echo 'serverurl=unix:///tmp/sd.sock'
  for i in $(seq 1 $N); do echo "[program:s$i]"; echo "command=/h/leaf.sh s$i"; echo 'autostart=true'; done
} > /tmp/sd.conf
supervisord -c /tmp/sd.conf >/dev/null 2>&1
for i in $(seq 1 600); do [ "$(ls /ready|wc -l)" -ge "$N" ] && break; sleep 0.05; done; sleep 3
SUP=$(pgrep -f "bin/supervisord" | head -1); SUPPSS=$(pss $SUP)
TOT=0; for d in /proc/[0-9]*; do TOT=$((TOT+$(pss ${d#/proc/}))); done
echo "SUPV,N=$N,sup=${SUPPSS},total=${TOT},procs=$(ls -d /proc/[0-9]*|wc -l),ready=$(ls /ready|wc -l)"
