#!/usr/bin/env bash
# USE CASE: a completed one-shot stays `done` across a restart.
#
# WHAT THIS TESTS (real dogfooding bug)
#   On a fresh start, one-shots that ran to completion report done/healthy. After
#   `sysg restart -p <project>` they flipped to stopped/warn, dragging the whole
#   project to WARN — observed live on a real project where build, migrations and
#   postgres all regressed while the project was in fact perfectly healthy.
#
#   A restart sets the manual-stop / restart-suppress flags to tear the old
#   instance down. The monitor thread saw the one-shot exit with those flags set
#   and persisted `Stopped` — overwriting the `ExitedSuccessfully` the start path
#   had just written. The flag means "a restart is in progress", NOT "the user
#   stopped this", and a clean exit from a one-shot is a COMPLETION.
#
#   This is not merely cosmetic: `Stopped` reads as a FAILED dependency, so
#   anything with `condition: completed` on that unit refuses to start.
#
#   `probe` declares NO restart_policy (like a `redis-cli ping` health probe) and
#   `build` declares `never` — both are one-shots, so keying only off `never`
#   would miss the probe. `web` is `always` and must still be Running.
#
# EXPECTED OUTCOME
#   After restart: probe and build are `done`, web is `running`, and the project
#   is NOT in WARN.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "initial start: one-shots complete"
sysg start --config "$CONFIG" --daemonize >/dev/null 2>&1
check "$?" "project started"
sleep 6

S1="$(sysg status --format json 2>/dev/null)"
[ "$(unit_field "$S1" probe state)" = "done" ]
check "$?" "probe is done after start (baseline)"
[ "$(unit_field "$S1" build state)" = "done" ]
check "$?" "build is done after start (baseline)"
[ "$(unit_field "$S1" web state)" = "running" ]
check "$?" "web is running after start (baseline)"

section "restart the whole project"
RESTART_OUTPUT="$(sysg restart -p oneshot 2>&1)"
RESTART_RC=$?
printf '%s\n' "$RESTART_OUTPUT"
echo "restart rc: $RESTART_RC"
[ "$RESTART_RC" -eq 0 ]
check "$?" "restart accepts delayed one-shot completion"
sleep 10

echo "--- status after restart ---"
sysg status 2>/dev/null | head -30

S2="$(sysg status --format json 2>/dev/null)"
PROBE="$(unit_field "$S2" probe state)"
BUILD="$(unit_field "$S2" build state)"
WEB="$(unit_field "$S2" web state)"
echo "probe=$PROBE build=$BUILD web=$WEB"

[ "$PROBE" = "done" ]
check "$?" "probe is STILL done after restart (was stopped)"
[ "$BUILD" = "done" ]
check "$?" "build is STILL done after restart (was stopped)"
[ "$WEB" = "running" ]
check "$?" "web is running after restart"

section "a completed one-shot is not a failed dependency"
[ "$(unit_field "$S2" probe health)" != "warn" ]
check "$?" "probe is not warn"
[ "$(unit_field "$S2" build health)" != "warn" ]
check "$?" "build is not warn"
[ "$(unit_field "$S2" probe health)" = "healthy" ]
check "$?" "probe is healthy"
[ "$(unit_field "$S2" build health)" = "healthy" ]
check "$?" "build is healthy"

sysg stop --supervisor >/dev/null 2>&1
finish
