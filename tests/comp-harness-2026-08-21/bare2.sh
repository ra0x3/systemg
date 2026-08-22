#!/bin/bash
N="$1"; mkdir -p /ready; rm -f /ready/*
pss(){ awk '/^Pss:/{s+=$2} END{print s+0}' /proc/$1/smaps_rollup 2>/dev/null; }
for i in $(seq 1 $N); do setsid /h/leaf.sh s$i >/dev/null 2>&1 & done
for i in $(seq 1 600); do [ "$(ls /ready|wc -l)" -ge "$N" ] && break; sleep 0.05; done; sleep 3
TOT=0; for d in /proc/[0-9]*; do TOT=$((TOT+$(pss ${d#/proc/}))); done
echo "BARE,N=$N,total=${TOT},procs=$(ls -d /proc/[0-9]*|wc -l)"
