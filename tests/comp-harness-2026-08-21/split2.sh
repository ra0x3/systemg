#!/bin/bash
export PATH="$HOME/.local/bin:$HOME/.sysg/bin:$PATH"
N="$1"; FORM="$2"; mkdir -p /ready; rm -f /ready/*
pss(){ awk '/^Pss:/{s+=$2} END{print s+0}' /proc/$1/smaps_rollup 2>/dev/null; }
{ echo 'version: "2"'; echo 'logs:'; echo '  sink: none'; echo 'services:'
  for i in $(seq 1 $N); do echo "  s$i:"
    if [ "$FORM" = exec ]; then
      echo "    exec: [\"/h/leaf.sh\", \"s$i\"]"
    else
      echo "    command: \"/h/leaf.sh s$i\""
    fi
  done; } > /tmp/c.yaml
sysg start -c /tmp/c.yaml --daemonize >/dev/null 2>&1
for i in $(seq 1 600); do [ "$(ls /ready|wc -l)" -ge "$N" ] && break; sleep 0.05; done; sleep 3
SUP=$(pgrep -x sysg | head -1); SUPPSS=$(pss $SUP)
KIDS=0; NK=0
for p in $(pgrep -P $SUP); do KIDS=$((KIDS+$(pss $p))); NK=$((NK+1)); done
TOT=0; for d in /proc/[0-9]*; do TOT=$((TOT+$(pss ${d#/proc/}))); done
echo "SPLIT,$FORM,N=$N,sup=${SUPPSS},children=${NK}:${KIDS},total=${TOT},procs=$(ls -d /proc/[0-9]*|wc -l)"
