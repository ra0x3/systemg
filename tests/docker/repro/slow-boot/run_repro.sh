#!/usr/bin/env bash
# Reproduces the slow-boot / wedged-control-socket bug.
#
# The FAST path is fine: with no supervisor running, `start --daemonize` forks,
# the child binds the control socket before starting services, and the parent
# returns in ~50ms. That is NOT where the frustrating wait comes from.
#
# The SLOW path is what users actually hit. When a supervisor is ALREADY running
# (the normal case -- you are restarting a stack that is up, or wedged),
# `sysg start --daemonize` takes the branch at main.rs:441 and instead sends
# ControlCommand::AddProject over the control socket, wrapped in
# with_progress_spinner("Starting", ...).
#
# supervisor.rs:1756 handles AddProject SYNCHRONOUSLY: add_project_config() runs
# the entire serial boot -- depends_on barrier, pre_start, wait_for_health_check
# (retries x interval), grace_period -- and only THEN sends a reply. The listener
# (supervisor.rs:1424, set_nonblocking(false)) accepts one connection at a time.
#
# Consequences, both asserted below:
#   1. `sysg start` blocks for the full boot duration behind one opaque spinner
#      that names no service and shows no progress.
#   2. the control socket is BLOCKED for that whole window -- so `sysg status`,
#      the one command you'd reach for to find out what is happening, also hangs.
#      That is the "infinite buggy state": sysg cannot tell you about itself.
#
# Also asserted: the depends_on barrier + pre_start duplicate build.sh.
#
# Nothing here double-forks; this is independent of the lost-service bug.
set -u

CONFIG=/repro/sysg.config.yaml
ANCHOR=/repro/anchor.config.yaml
export HOME=/root

PASS=0
FAIL=0
section() { printf '\n========== %s ==========\n' "$1"; }
check()   { if [ "$1" = "0" ]; then echo "PASS: $2"; PASS=$((PASS+1)); else echo "FAIL: $2"; FAIL=$((FAIL+1)); fi; }

rm -f /tmp/build.trace

section "bring a supervisor up first (the normal, already-running case)"
sysg start --config "$ANCHOR" --daemonize
sleep 2
sysg status --format json >/dev/null 2>&1
check "$?" "precondition: supervisor is up and answering on the control socket"

section "now 'sysg start' a second project -> goes through AddProject over IPC"
START_NS=$(date +%s%N)
sysg start --config "$CONFIG" --log-level debug --daemonize &
START_CLI_PID=$!

# While the CLI spins on "Starting", is the control socket still usable?
# Give the supervisor a beat to enter add_project_config, then probe it.
sleep 4
PROBE_NS=$(date +%s%N)
timeout 10 sysg status --format json >/dev/null 2>&1
PROBE_RC=$?
PROBE_MS=$(( ($(date +%s%N) - PROBE_NS) / 1000000 ))
echo "sysg status during boot: rc=$PROBE_RC after ${PROBE_MS} ms"

# rc=124 is `timeout` killing a hung status.
[ "$PROBE_RC" -eq 0 ] && [ "$PROBE_MS" -lt 3000 ]
check "$?" "sysg status stays responsive while another project boots"

wait "$START_CLI_PID"
BLOCKED_MS=$(( ($(date +%s%N) - START_NS) / 1000000 ))
echo "sysg start blocked the CLI for: ${BLOCKED_MS} ms"

# barrier(6s) + pre_start(6s) + health checks(~6s) + grace(3s). Any of this in
# the foreground of `start` is boot work the CLI should not be waiting on.
[ "$BLOCKED_MS" -lt 3000 ]
check "$?" "start --daemonize returns promptly instead of blocking on serial boot"

section "build.sh invocations"
cat /tmp/build.trace 2>/dev/null || echo "(no trace)"
BUILDS=$(grep -c 'build start' /tmp/build.trace 2>/dev/null || echo 0)
echo "build.sh invocations: $BUILDS"
[ "$BUILDS" -le 1 ]
check "$?" "build.sh runs once, not once per (barrier + pre_start)"

section "what was the CLI actually waiting on?"
grep -iE "pre-start|health check|grace" /root/.local/share/systemg/logs/supervisor.log 2>/dev/null | tail -10

section "RESULT"
echo "passed: $PASS  failed: $FAIL"
if [ "$FAIL" -eq 0 ]; then
  echo "=> GREEN: slow-boot fixed."
  exit 0
else
  echo "=> RED: slow-boot reproduced / not fixed."
  exit 1
fi
