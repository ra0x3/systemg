#!/bin/bash
DELAY="$1"; STARTSECS="$2"; mkdir -p /ready; rm -f /ready/*
{ echo '[unix_http_server]'; echo 'file=/tmp/sd.sock'; echo '[supervisord]'; echo 'logfile=/tmp/sd.log'
  echo '[rpcinterface:supervisor]'
  echo 'supervisor.rpcinterface_factory = supervisor.rpcinterface:make_main_rpcinterface'
  echo '[supervisorctl]'; echo 'serverurl=unix:///tmp/sd.sock'
  echo '[program:w]'; echo "command=/h/slowN.sh $DELAY"; echo 'autostart=true'
  echo "startsecs=$STARTSECS"; } > /tmp/sd.conf
T0=$(date +%s.%N); supervisord -c /tmp/sd.conf >/dev/null 2>&1
UP=""; for i in $(seq 1 400); do supervisorctl -c /tmp/sd.conf status w 2>/dev/null | grep -q RUNNING && { UP=$(awk -v a=$T0 -v b=$(date +%s.%N) 'BEGIN{printf "%.2f",b-a}'); break; }; sleep 0.1; done
R=""; for i in $(seq 1 400); do [ -f /ready/w ] && { R=$(awk -v a=$T0 -v b=$(date +%s.%N) 'BEGIN{printf "%.2f",b-a}'); break; }; sleep 0.1; done
echo "SS,delay=${DELAY}s,startsecs=${STARTSECS},reported_up=${UP}s,usable=${R}s,lie=$(awk -v u=$UP -v r=$R 'BEGIN{printf "%+.2f",r-u}')s"
