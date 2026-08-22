#!/bin/bash
export PATH="$HOME/.local/bin:$HOME/.sysg/bin:$PATH"
MODE="$1"; mkdir -p /ready; rm -f /ready/*
n() { pgrep -c -x "sleep" 2>/dev/null || echo 0; }
CMD='sh -c "echo r > /ready/w; exec sleep 3600 # MARKER_SVC"'
case "$MODE" in
sysg)
  printf 'version: "2"\nlogs:\n  sink: none\nservices:\n  w:\n    command: "/h/svc1.sh"\n' > /tmp/s.yaml
  sysg start -c /tmp/s.yaml --daemonize >/dev/null 2>&1; sleep 3
  echo "  running before crash: $(n)"
  SPID=$(pgrep -x sysg | head -1); echo "  killing supervisor pid $SPID"
  kill -9 "$SPID" 2>/dev/null; sleep 2
  echo "  services survived supervisor death: $(n)"
  sysg start -c /tmp/s.yaml --daemonize >/dev/null 2>&1; sleep 4
  echo "  after supervisor restart: $(n)   (1=re-adopted, 2=DOUBLE-STARTED, 0=lost)"
  ;;
supervisor)
  { echo '[unix_http_server]'; echo 'file=/tmp/sd.sock'; echo '[supervisord]'
    echo 'logfile=/tmp/sd.log'; echo 'pidfile=/tmp/sd.pid'
    echo '[rpcinterface:supervisor]'
    echo 'supervisor.rpcinterface_factory = supervisor.rpcinterface:make_main_rpcinterface'
    echo '[supervisorctl]'; echo 'serverurl=unix:///tmp/sd.sock'
    echo '[program:w]'; echo 'command=/h/svc1.sh'
    echo 'autostart=true'; } > /tmp/sd.conf
  supervisord -c /tmp/sd.conf >/dev/null 2>&1; sleep 3
  echo "  running before crash: $(n)"
  SPID=$(cat /tmp/sd.pid); echo "  killing supervisor pid $SPID"
  kill -9 "$SPID" 2>/dev/null; sleep 2
  echo "  services survived supervisor death: $(n)"
  rm -f /tmp/sd.pid
  supervisord -c /tmp/sd.conf >/dev/null 2>&1; sleep 4
  echo "  after supervisor restart: $(n)   (1=re-adopted, 2=DOUBLE-STARTED, 0=lost)"
  ;;
esac
