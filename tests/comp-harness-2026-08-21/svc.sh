#!/bin/sh
# $1 = service name. INIT_MS of simulated startup work, then stamp ready.
n="$1"
sleep "${INIT_SECS:-0.3}"
date +%s.%N > "/ready/$n"
exec sleep 3600
