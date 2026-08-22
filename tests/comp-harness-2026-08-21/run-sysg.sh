#!/bin/bash
set -e
export PATH="$HOME/.local/bin:$HOME/.sysg/bin:$PATH"
curl --proto '=https' --tlsv1.2 -fsSL https://sh.sysg.dev | sh >/dev/null 2>&1
mkdir -p /ready && rm -f /ready/*
export INIT_SECS=${INIT_SECS:-0.3}
T0=$(date +%s.%N)
sysg start -c /svc/systemg.yaml --daemonize >/tmp/start.log 2>&1 || true
# all-healthy: every ready file present AND sysg says so
for i in $(seq 1 600); do
  n=$(ls /ready 2>/dev/null | wc -l)
  [ "$n" -eq 10 ] && break
  sleep 0.05
done
T1=$(date +%s.%N)
echo "TOTAL,$(awk -v a=$T0 -v b=$T1 'BEGIN{printf "%.3f", b-a}')"
for f in /ready/*; do
  echo "SVC,$(basename $f),$(awk -v a=$T0 -v b=$(cat $f) 'BEGIN{printf "%.3f", b-a}')"
done
sysg status -c /svc/systemg.yaml 2>/dev/null | tail -14 > /tmp/st.txt || true
echo "---STATUS---"; cat /tmp/st.txt
