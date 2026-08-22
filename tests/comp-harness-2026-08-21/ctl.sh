#!/bin/bash
export PATH="$HOME/.local/bin:$HOME/.sysg/bin:$PATH"
curl -fsSL https://sh.sysg.dev | sh >/dev/null 2>&1
for cfg in nohealth flat; do
  rm -rf /ready; mkdir -p /ready
  sysg purge --force >/dev/null 2>&1 || true
  T0=$(date +%s.%N)
  sysg start -c /svc/$cfg.yaml --daemonize >/dev/null 2>&1 || true
  for i in $(seq 1 600); do [ "$(ls /ready 2>/dev/null | wc -l)" -eq 10 ] && break; sleep 0.05; done
  T1=$(date +%s.%N)
  echo "== $cfg TOTAL $(awk -v a=$T0 -v b=$T1 'BEGIN{printf "%.3f", b-a}')"
  for f in /ready/*; do echo "   $(basename $f) $(awk -v a=$T0 -v b=$(cat $f) 'BEGIN{printf "%.3f", b-a}')"; done | sort -k2 -n
done
