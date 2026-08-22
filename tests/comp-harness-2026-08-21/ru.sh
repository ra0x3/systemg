#!/bin/bash
# RU harness. Measures total PSS of every process in the container except the
# measuring shell, so attribution disputes inside the container disappear.
export PATH="$HOME/.local/bin:$HOME/.sysg/bin:$PATH"
MODE="$1"; N="${2:-10}"
mkdir -p /ready; rm -f /ready/*
SELF=$$

pss_total() {  # sum PSS over all pids except this shell + its children
  local sum=0
  for d in /proc/[0-9]*; do
    pid=${d#/proc/}
    [ "$pid" = "$SELF" ] && continue
    case "$(cat $d/comm 2>/dev/null)" in ru.sh|bash|sh) [ "$pid" = "$SELF" ] && continue;; esac
    p=$(awk '/^Pss:/{s+=$2} END{print s+0}' $d/smaps_rollup 2>/dev/null)
    sum=$((sum + ${p:-0}))
  done
  echo $sum
}
nproc_total() { ls -d /proc/[0-9]* 2>/dev/null | wc -l; }

svc() { printf 'sleep 0.2; date +%%s.%%N > /ready/%s; exec sleep 3600' "$1"; }

case "$MODE" in
bare)
  # one-shot launcher: forks N services, then EXITS. no resident parent.
  for i in $(seq 1 $N); do setsid sh -c "$(svc s$i)" >/dev/null 2>&1 & done
  ;;
sysg)
  { echo 'version: "2"'; echo 'logs:'; echo '  sink: none'; echo 'services:';
    for i in $(seq 1 $N); do echo "  s$i:"; echo "    command: \"sh -c '$(svc s$i)'\""; done; } > /tmp/sysg.yaml
  sysg start -c /tmp/sysg.yaml --daemonize >/dev/null 2>&1
  ;;
supervisor)
  { echo '[unix_http_server]'; echo 'file=/tmp/sd.sock'; echo '[supervisord]';
    echo 'logfile=/tmp/sd.log'; echo 'pidfile=/tmp/sd.pid';
    echo '[rpcinterface:supervisor]';
    echo 'supervisor.rpcinterface_factory = supervisor.rpcinterface:make_main_rpcinterface';
    echo '[supervisorctl]'; echo 'serverurl=unix:///tmp/sd.sock';
    for i in $(seq 1 $N); do echo "[program:s$i]"; echo "command=sh -c \"$(svc s$i | sed 's/%/%%/g')\"";
      echo 'autostart=true'; done; } > /tmp/sd.conf
  supervisord -c /tmp/sd.conf >/dev/null 2>&1
  ;;
esac

for i in $(seq 1 400); do [ "$(ls /ready 2>/dev/null | wc -l)" -ge "$N" ] && break; sleep 0.05; done
sleep 3   # settle
echo "RU,$MODE,$N,$(pss_total),$(nproc_total)"
