#!/bin/bash
set -e
now() { date +%s.%N; }
el() { awk -v a="$1" -v b="$2" 'BEGIN{printf "%.2f", b-a}'; }
RXB() { cat /sys/class/net/eth0/statistics/rx_bytes 2>/dev/null || echo 0; }
R0=$(RXB); export DEBIAN_FRONTEND=noninteractive
case "$1" in
supervisor-nopy)
  T0=$(now)
  apt-get update -qq >/dev/null 2>&1
  apt-get install -y -qq python3 python3-pip >/dev/null 2>&1
  pip install -q --break-system-packages supervisor >/dev/null 2>&1
  T1=$(now); supervisord -v >/dev/null 2>&1; T2=$(now)
  cat > /tmp/sd.conf <<'CFG'
[unix_http_server]
file=/tmp/sd.sock
[supervisord]
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
  supervisord -c /tmp/sd.conf >/dev/null 2>&1
  for i in $(seq 1 100); do supervisorctl -c /tmp/sd.conf status probe 2>/dev/null | grep -q RUNNING && break; sleep 0.1; done
  T3=$(now); OK=$(pgrep -f "sleep 600" >/dev/null && echo yes || echo no)
  ;;
docker-pkg)
  apt-get update -qq >/dev/null 2>&1
  apt-get install -y -qq curl ca-certificates gnupg >/dev/null 2>&1
  install -m 0755 -d /etc/apt/keyrings
  curl -fsSL https://download.docker.com/linux/debian/gpg -o /etc/apt/keyrings/docker.asc
  chmod a+r /etc/apt/keyrings/docker.asc
  echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/debian bookworm stable" > /etc/apt/sources.list.d/docker.list
  R0=$(RXB)
  T0=$(now)
  apt-get update -qq >/dev/null 2>&1
  apt-get install -y -qq docker-ce docker-ce-cli containerd.io docker-compose-plugin >/dev/null 2>&1
  T1=$(now); docker compose version >/dev/null 2>&1; T2=$(now)
  T3=$T2; OK="n/a-container"
  ;;
esac
echo "RESULT,$1,$(el $T0 $T1),$(el $T1 $T2),$(el $T0 $T3),$OK,$(awk -v a=$R0 -v b=$(RXB) 'BEGIN{print b-a}')"
