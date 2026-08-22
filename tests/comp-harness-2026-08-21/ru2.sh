#!/bin/bash
# RU2: steady-state resource overhead with DEFAULT logging enabled.
#
# ru.sh measured a freshly booted supervisor with `logs: sink: none` and silent
# services, so it could not see the caches that grow with N and with time:
# the live-log ring (256 KiB per service per stream) and the metrics ring
# (720 min at 1 Hz). RU2 keeps the run alive and makes every service emit
# output, so those caches reach steady state inside the measured window.
#
# Emits one CSV row per checkpoint:
#   RU2,mode,N,t_secs,total_pss_kb,nproc,sup_pss_kb,wrap_pss_kb,leaf_pss_kb
export PATH="$HOME/.local/bin:$HOME/.sysg/bin:$PATH"
MODE="$1"; N="${2:-10}"; DURATION="${3:-600}"
CHECKPOINTS="${CHECKPOINTS:-30 60 180 360 600}"
mkdir -p /ready /blog; rm -f /ready/* /blog/*
SELF=$$

pss() { local v; v=$(awk '/^Pss:/{s+=$2} END{print s+0}' /proc/$1/smaps_rollup 2>/dev/null); echo "${v:-0}"; }

pss_total() {
  local sum=0 p
  for d in /proc/[0-9]*; do
    pid=${d#/proc/}
    [ "$pid" = "$SELF" ] && continue
    p=$(awk '/^Pss:/{s+=$2} END{print s+0}' $d/smaps_rollup 2>/dev/null)
    sum=$((sum + ${p:-0}))
  done
  echo $sum
}
nproc_total() { ls -d /proc/[0-9]* 2>/dev/null | wc -l; }

# A chatty service: ~10 lines/s of ~40 bytes fills a 256 KiB live-log ring in
# about 11 minutes, and a file sink continuously.
svc() { printf 'sleep 0.2; date +%%s.%%N > /ready/%s; while :; do date +%%s.%%N; sleep 0.1; done' "$1"; }

case "$MODE" in
bare)
  for i in $(seq 1 $N); do
    setsid sh -c "$(svc s$i)" >>/blog/s$i.log 2>&1 &
  done
  ;;
sysg)
  { echo 'version: "2"'; echo 'services:';
    for i in $(seq 1 $N); do echo "  s$i:"; echo "    command: \"sh -c '$(svc s$i)'\""; done; } > /tmp/sysg.yaml
  sysg start -c /tmp/sysg.yaml --daemonize >/dev/null 2>&1
  ;;
supervisor)
  { echo '[unix_http_server]'; echo 'file=/tmp/sd.sock'; echo '[supervisord]';
    echo 'logfile=/tmp/sd.log'; echo 'pidfile=/tmp/sd.pid'; echo 'childlogdir=/blog';
    echo '[rpcinterface:supervisor]';
    echo 'supervisor.rpcinterface_factory = supervisor.rpcinterface:make_main_rpcinterface';
    echo '[supervisorctl]'; echo 'serverurl=unix:///tmp/sd.sock';
    for i in $(seq 1 $N); do echo "[program:s$i]"; echo "command=sh -c \"$(svc s$i | sed 's/%/%%/g')\"";
      echo 'autostart=true'; done; } > /tmp/sd.conf
  supervisord -c /tmp/sd.conf >/dev/null 2>&1
  ;;
esac

for i in $(seq 1 400); do [ "$(ls /ready 2>/dev/null | wc -l)" -ge "$N" ] && break; sleep 0.05; done

START=$(date +%s)
for t in $CHECKPOINTS; do
  [ "$t" -gt "$DURATION" ] && continue
  while [ $(( $(date +%s) - START )) -lt "$t" ]; do sleep 1; done
  SUP=0; WRAP=0; LEAF=0
  case "$MODE" in
    sysg)
      S=$(pgrep -x sysg | head -1)
      SUP=$(pss ${S:-0})
      for p in $(pgrep -P ${S:-0} 2>/dev/null); do
        WRAP=$((WRAP + $(pss $p)))
        for c in $(pgrep -P $p 2>/dev/null); do LEAF=$((LEAF + $(pss $c))); done
      done
      ;;
    supervisor)
      S=$(pgrep -f "supervisord -c" | head -1)
      SUP=$(pss ${S:-0})
      for p in $(pgrep -P ${S:-0} 2>/dev/null); do LEAF=$((LEAF + $(pss $p))); done
      ;;
  esac
  echo "RU2,$MODE,$N,$t,$(pss_total),$(nproc_total),$SUP,$WRAP,$LEAF"
done
