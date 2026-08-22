#!/bin/bash
export PATH="$HOME/.local/bin:$HOME/.sysg/bin:$PATH"
N="$1"; mkdir -p /ready; rm -f /ready/*
pss(){ awk '/^Pss:/{s+=$2} END{print s+0}' /proc/$1/smaps_rollup 2>/dev/null; }
{ echo 'version: "2"'; echo 'logs:'; echo '  sink: none'; echo 'services:'
  for i in $(seq 1 $N); do echo "  s$i:"; echo "    command: \"sh -c 'sleep 0.2; date +%s.%N > /ready/s$i; exec sleep 3600'\""; done; } > /tmp/c.yaml
sysg start -c /tmp/c.yaml --daemonize >/dev/null 2>&1
for i in $(seq 1 400); do [ "$(ls /ready|wc -l)" -ge "$N" ] && break; sleep 0.05; done; sleep 3
SUP=$(pgrep -x sysg | head -1)
SUPPSS=$(pss $SUP)
WRAP=0; NW=0
for p in $(pgrep -P $SUP); do WRAP=$((WRAP+$(pss $p))); NW=$((NW+1)); done
LEAF=0; NL=0
for p in $(pgrep -x sleep); do LEAF=$((LEAF+$(pss $p))); NL=$((NL+1)); done
echo "SPLIT,N=$N,supervisor_pss=${SUPPSS},wrappers=${NW}:${WRAP},leaves=${NL}:${LEAF}"
