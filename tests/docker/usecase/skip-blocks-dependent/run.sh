#!/usr/bin/env bash
# USE CASE: a service depending on a SKIPPED service must NOT run.
#
# WHAT THIS TESTS (0.54.2 bug: skip ignored, dependency propagation)
#   Service B depends_on service A; A has skip: true. A must not run (skipped),
#   and because B's dependency was never satisfied, B must NOT run either — a
#   skipped dependency is an unmet dependency, not a silent pass. An independent
#   service C runs normally.
#
# HARD INVARIANTS
#   - A (skip: true) has no process,
#   - B (depends on skipped A) has NO process — its dep was never satisfied,
#   - status shows A skipped and B NOT running (blocked/not-started),
#   - C (independent) runs normally.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "start (A is skip:true, B depends_on A, C is independent)"
sysg start --config "$CONFIG" --daemonize >/tmp/start.err 2>&1
cat /tmp/start.err | grep -v WARN | head
sleep 3

section "A (skipped) did not run"
# match the actual service command (echo A_LINE), not run.sh's own text
[ "$(pgrep -f 'echo A_LINE' | wc -l | tr -d ' ')" = "0" ]
check "$?" "no process for skipped service A"

section "B did NOT run (its dependency A was skipped = unsatisfied)"
NB="$(pgrep -f 'echo B_LINE' | wc -l | tr -d ' ')"
echo "B procs: $NB (expect 0 — B depends on the skipped A)"
[ "$NB" = "0" ]
check "$?" "B did not run because its dependency was skipped"

section "C (independent) runs normally"
[ "$(pgrep -f 'echo C_LINE' | wc -l | tr -d ' ')" -ge 1 ]
check "$?" "independent service C is running"

section "status: A skipped, B not running, C running"
S="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
for svc in a b c; do echo "  $svc -> $(unit_field "$S" "$svc" state dev)"; done
[ "$(unit_field "$S" a state dev)" != "running" ]
check "$?" "status: A is not running (skipped)"
B_STATE="$(unit_field "$S" b state dev)"
[ "$B_STATE" != "running" ]
check "$?" "status: B is not running (blocked by skipped dep; state=$B_STATE)"
CP="$(unit_field "$S" c state dev)"
[ "$CP" = "running" ]
check "$?" "status: C is running"

sysg stop --supervisor >/dev/null 2>&1
finish
