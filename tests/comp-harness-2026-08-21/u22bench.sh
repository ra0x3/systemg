#!/bin/bash
export DEBIAN_FRONTEND=noninteractive
now(){ date +%s.%N; }; el(){ awk -v a="$1" -v b="$2" 'BEGIN{printf "%.2f",b-a}'; }
RX(){ cat /sys/class/net/eth0/statistics/rx_bytes 2>/dev/null||echo 0; }
R0=$(RX); T0=$(now)
case "$1" in
sysg)
  curl --proto '=https' --tlsv1.2 -fsSL https://sh.sysg.dev | sh >/dev/null 2>&1
  T1=$(now); export PATH="$HOME/.local/bin:$HOME/.sysg/bin:$PATH"
  printf 'version: "2"\nservices:\n  p:\n    command: "sleep 600"\n' > /tmp/c.yaml
  sysg start -c /tmp/c.yaml --daemonize >/dev/null 2>&1
  for i in $(seq 1 200); do pgrep -f "sleep 600" >/dev/null && break; sleep 0.05; done ;;
supervisor)
  apt-get update -qq >/dev/null 2>&1; apt-get install -y -qq supervisor >/dev/null 2>&1
  T1=$(now)
  printf '[program:p]\ncommand=sleep 600\nautostart=true\n' > /etc/supervisor/conf.d/p.conf
  supervisord -c /etc/supervisor/supervisord.conf >/dev/null 2>&1
  for i in $(seq 1 200); do pgrep -f "sleep 600" >/dev/null && break; sleep 0.05; done ;;
compose)
  install -m 0755 -d /etc/apt/keyrings
  curl -fsSL https://download.docker.com/linux/ubuntu/gpg -o /etc/apt/keyrings/docker.asc 2>/dev/null
  chmod a+r /etc/apt/keyrings/docker.asc
  echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/ubuntu jammy stable" > /etc/apt/sources.list.d/docker.list
  R0=$(RX); T0=$(now)
  apt-get update -qq >/dev/null 2>&1
  apt-get install -y -qq docker-ce docker-ce-cli containerd.io docker-compose-plugin >/dev/null 2>&1
  T1=$(now) ;;
esac
T2=$(now)
echo "U22,$1,install=$(el $T0 $T1)s,to_ready=$(el $T0 $T2)s,rx=$(awk -v a=$R0 -v b=$(RX) 'BEGIN{print b-a}')"
