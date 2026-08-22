#!/bin/bash
set -e
now() { date +%s.%N; }
el() { awk -v a="$1" -v b="$2" 'BEGIN{printf "%.2f", b-a}'; }
RXB() { cat /sys/class/net/eth0/statistics/rx_bytes 2>/dev/null || echo 0; }
R0=$(RXB)

case "$1" in
sysg)
  T0=$(now)
  curl --proto '=https' --tlsv1.2 -fsSL https://sh.sysg.dev | sh >/tmp/i.log 2>&1
  T1=$(now)
  export PATH="$HOME/.local/bin:$HOME/.sysg/bin:$PATH"
  sysg --version >/dev/null 2>&1
  T2=$(now)
  mkdir -p /tmp/svc; printf 'version: "2"\nservices:\n  probe:\n    command: "sleep 600"\n' > /tmp/svc/systemg.yaml
  cd /tmp/svc && sysg start -c /tmp/svc/systemg.yaml --daemonize >/tmp/s.log 2>&1
  for i in $(seq 1 100); do sysg status -c /tmp/svc/systemg.yaml 2>/dev/null | grep -qiE "running|healthy" && break; sleep 0.1; done
  T3=$(now)
  OK=$(pgrep -f "sleep 600" >/dev/null && echo yes || echo no)
  ;;
supervisor)
  T0=$(now)
  pip install -q --break-system-packages supervisor >/tmp/i.log 2>&1
  T1=$(now)
  supervisord -v >/dev/null 2>&1
  T2=$(now)
  cat > /tmp/sd.conf <<'CFG'
[unix_http_server]
file=/tmp/sd.sock
[supervisord]
nodaemon=false
logfile=/tmp/sd.log
pidfile=/tmp/sd.pid
[rpcinterface:supervisor]
supervisor.rpcinterface_factory = supervisor.rpcinterface:make_main_rpcinterface
[supervisorctl]
serverurl=unix:///tmp/sd.sock
[program:probe]
command=sleep 600
autostart=true
CFG
  supervisord -c /tmp/sd.conf >/tmp/s.log 2>&1
  for i in $(seq 1 100); do supervisorctl -c /tmp/sd.conf status probe 2>/dev/null | grep -q RUNNING && break; sleep 0.1; done
  T3=$(now)
  OK=$(pgrep -f "sleep 600" >/dev/null && echo yes || echo no)
  ;;
esac
echo "RESULT,$1,$(el $T0 $T1),$(el $T1 $T2),$(el $T0 $T3),$OK,$(awk -v a=$R0 -v b=$(RXB) 'BEGIN{print b-a}')"
