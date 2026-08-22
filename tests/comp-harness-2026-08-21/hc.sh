#!/bin/bash
export PATH="$HOME/.local/bin:$HOME/.sysg/bin:$PATH"
MODE="$1"; mkdir -p /ready; rm -f /ready/*
{ echo 'version: "2"'; echo 'logs:'; echo '  sink: none'; echo 'services:'; echo '  w:'
  echo '    command: "/h/svc2.sh"'
  if [ "$MODE" = "hc" ]; then
    echo '    deployment:'; echo '      health_check:'
    echo '        command: "test -f /ready/w"'; echo '        interval: "1s"'
  fi; } > /tmp/s.yaml
sysg start -c /tmp/s.yaml --daemonize >/dev/null 2>&1; sleep 4
echo "  [$MODE] BEFORE : $(ps -eo pid,ppid,args | grep -c '[s]vc2.sh') procs -> $(ps -eo pid,args | grep '[s]vc2.sh' | awk '{printf "%s ", $1}')"
kill -9 $(pgrep -x sysg | head -1); sleep 2
echo "  [$MODE] CP DEAD: $(ps -eo pid,args | grep -c '[s]vc2.sh') procs -> $(ps -eo pid,args | grep '[s]vc2.sh' | awk '{printf "%s ", $1}')"
sysg start -c /tmp/s.yaml --daemonize >/dev/null 2>&1; sleep 8
echo "  [$MODE] AFTER  : $(ps -eo pid,args | grep -c '[s]vc2.sh') procs -> $(ps -eo pid,args | grep '[s]vc2.sh' | awk '{printf "%s ", $1}')"
