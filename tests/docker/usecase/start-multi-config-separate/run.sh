#!/usr/bin/env bash
# USE CASE: three projects from three SEPARATE config files under one supervisor.
#
# WHAT THIS TESTS
#   The real production topology: each project lives in its own file
#   (alpha.yaml, beta.yaml, gamma.yaml) and is started with its own
#   `sysg start -c <file>`. The FIRST start boots the supervisor; each
#   subsequent `start -c <otherfile>` registers its project into the SAME
#   running supervisor (AddProject). One supervisor hosts all three, however
#   many files registered them. This is where the cross-project prod bugs lived.
#
# EXPECTED OUTCOME
#   - After three separate `start -c` calls, all three services (alpha_svc,
#     beta_svc, gamma_svc) are running with distinct live pids (ps agrees).
#   - `sysg status` with NO -c (asks the resident supervisor) lists ALL THREE
#     units, each under its own project.
#   - Each project has its own on-disk projects/{id}/pid.xml; the three are
#     distinct.
#   - Exactly one supervisor process exists (not three).
set -u
. /usecase/lib.sh

STATE_DIR="$HOME/.local/share/systemg"

section "register three projects from three separate files, one at a time"
sysg start --config /usecase/alpha.yaml --daemonize
check "$?" "start -c alpha.yaml exits 0 (boots supervisor + alpha)"
sleep 2
sysg start --config /usecase/beta.yaml --daemonize
check "$?" "start -c beta.yaml exits 0 (adds beta to running supervisor)"
sleep 2
sysg start --config /usecase/gamma.yaml --daemonize
check "$?" "start -c gamma.yaml exits 0 (adds gamma to running supervisor)"
sleep 3

section "the resident supervisor hosts all three projects"
STATUS="$(sysg status --format json 2>/dev/null)"
[ "$(unit_count "$STATUS")" = "3" ]
check "$?" "status (no -c) shows exactly three units"

A_PID="$(unit_field "$STATUS" alpha_svc pid alpha)"
B_PID="$(unit_field "$STATUS" beta_svc pid beta)"
G_PID="$(unit_field "$STATUS" gamma_svc pid gamma)"
echo "pids -> alpha:$A_PID beta:$B_PID gamma:$G_PID"

[ "$(unit_field "$STATUS" alpha_svc state alpha)" = "running" ]
check "$?" "alpha_svc running under project alpha"
[ "$(unit_field "$STATUS" beta_svc state beta)" = "running" ]
check "$?" "beta_svc running under project beta"
[ "$(unit_field "$STATUS" gamma_svc state gamma)" = "running" ]
check "$?" "gamma_svc running under project gamma"

section "each service pid is alive and distinct"
# Adaptive wait so a slow third-project boot does not read as a failure; the
# real signal is the isolation check below, not a timing race.
wait_alive() { for _ in 1 2 3 4 5 6 7 8 9 10; do pid_alive "$1" && return 0; sleep 0.5; done; return 1; }
wait_alive "$A_PID"; check "$?" "alpha_svc pid alive"
wait_alive "$B_PID"; check "$?" "beta_svc pid alive"
wait_alive "$G_PID"; check "$?" "gamma_svc pid alive"
[ "$A_PID" != "$B_PID" ] && [ "$B_PID" != "$G_PID" ] && [ "$A_PID" != "$G_PID" ]
check "$?" "the three pids are pairwise distinct"

section "each project owns its own state directory -- no service leakage"
[ -f "$STATE_DIR/projects/alpha/pid.xml" ]; check "$?" "projects/alpha/pid.xml exists"
[ -f "$STATE_DIR/projects/beta/pid.xml" ];  check "$?" "projects/beta/pid.xml exists"
[ -f "$STATE_DIR/projects/gamma/pid.xml" ]; check "$?" "projects/gamma/pid.xml exists"

# Each project's pid.xml must record ONLY its own service -- separately
# registered projects must not leak siblings' services into each other's files.
grep -q "gamma_svc" "$STATE_DIR/projects/gamma/pid.xml" 2>/dev/null \
  && ! grep -qE "alpha_svc|beta_svc" "$STATE_DIR/projects/gamma/pid.xml" 2>/dev/null
check "$?" "gamma/pid.xml records ONLY gamma_svc (no sibling leakage)"
grep -q "alpha_svc" "$STATE_DIR/projects/alpha/pid.xml" 2>/dev/null \
  && ! grep -qE "beta_svc|gamma_svc" "$STATE_DIR/projects/alpha/pid.xml" 2>/dev/null
check "$?" "alpha/pid.xml records ONLY alpha_svc (no sibling leakage)"

section "exactly one supervisor process (not three)"
# The resident supervisor runs as `sysg supervise ...` (after the daemonize
# re-exec). Match that exact form — NOT `--daemonize`, which also appears in the
# transient start CLIs and even this test's own shell command line.
SUP_COUNT="$(ps -eo args | grep -c "[s]ysg supervise")"
echo "supervisor process count: $SUP_COUNT"
[ "$SUP_COUNT" = "1" ]
check "$?" "exactly one resident supervisor"

sysg stop --supervisor >/dev/null 2>&1
finish
