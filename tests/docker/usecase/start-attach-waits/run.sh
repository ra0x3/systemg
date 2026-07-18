#!/usr/bin/env bash
# USE CASE: attaching a project to a running supervisor waits for its boot.
#
# WHAT THIS TESTS (real dogfooding bug)
#   `sysg start` behaves differently depending on whether a supervisor already
#   exists, and the user notices:
#     - NO supervisor  -> the CLI forks one, stays attached, and shows the boot's
#                         progress (health checks, dependency waits).
#     - supervisor UP   -> `AddProject` returned the instant the boot was QUEUED
#                         onto a background thread, printing "Project 'x' loaded"
#                         while nothing had started yet.
#
#   The async return is deliberate (a slow boot must not block the supervisor's
#   single-writer owner thread), but reporting "loaded" at queue time reports
#   SUCCESS FOR WORK THAT HAS NOT HAPPENED — every health check, dependency wait
#   and startup failure lands after the user is back at the prompt, and the exit
#   code cannot tell them whether the project actually came up.
#
#   `beta` has a `slowdep` that sleeps 8s with `bsvc` gated on its COMPLETION, so
#   a start that returns early is trivially distinguishable from one that waits.
#
# EXPECTED OUTCOME
#   The second (attaching) start does not return until beta has genuinely
#   booted: on return, bsvc is already running and slowdep is done.
set -u
. /usecase/lib.sh

section "first start: no supervisor, so this forks one"
sysg start --config /usecase/alpha.yaml --daemonize >/dev/null 2>&1
check "$?" "alpha started"
sleep 3
pgrep -x sysg >/dev/null
check "$?" "a supervisor is now resident"

section "second start ATTACHES to that supervisor and must wait for the boot"
START_EPOCH="$(date +%s)"
sysg start --config /usecase/beta.yaml --daemonize >/tmp/attach.out 2>&1
RC=$?
END_EPOCH="$(date +%s)"
ELAPSED=$((END_EPOCH - START_EPOCH))
echo "attach start rc=$RC elapsed=${ELAPSED}s"
cat /tmp/attach.out

[ "$RC" = "0" ]
check "$?" "the attaching start exits 0"

# slowdep sleeps 8s and bsvc is gated on its completion, so a start that
# genuinely waits cannot return in ~0s.
[ "$ELAPSED" -ge 7 ]
check "$?" "the start WAITED for the queued boot (${ELAPSED}s, expected >=7)"

section "on return, the project is genuinely up — not merely queued"
S="$(sysg status --format json 2>/dev/null)"
[ "$(unit_field "$S" bsvc state)" = "running" ]
check "$?" "bsvc is already running when the start returns"
[ "$(unit_field "$S" slowdep state)" = "done" ]
check "$?" "slowdep already completed when the start returns"

section "the first project is untouched"
[ "$(unit_field "$S" asvc state)" = "running" ]
check "$?" "alpha's service still running"

sysg stop --supervisor >/dev/null 2>&1
finish
