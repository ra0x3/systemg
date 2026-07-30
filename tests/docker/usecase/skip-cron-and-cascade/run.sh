#!/usr/bin/env bash
# USE CASE: `skip` means skipped for EVERY unit kind, including cron.
#
# WHAT THIS TESTS (real production bug, sysg 0.59.3)
#   A cron unit carrying `skip: true` reported `Done · Healthy · exit 0` and kept
#   reporting a fresh exit every time its expression came due. Two defects:
#
#     1. The cron path passed the service config with `skip` INTACT into
#        `start_service`, which suppressed the command (correct) but returned
#        `ServiceReadyState::CompletedSuccess` — indistinguishable from a real
#        clean run. The cron completion handler then stamped
#        `ExitedSuccessfully/0` over the `Skipped` lifecycle and wrote a
#        `Success` record into cron history, on EVERY boundary. The unit was
#        never executing, yet claimed a successful run every minute.
#
#     2. `CompletedSuccess` cannot express "did not fail" AND "does not satisfy
#        dependents" at once, so `cascade_restart` walked past a skipped root
#        and restarted its dependents behind a dependency that never came up.
#
#   The bulk-start path already held the right invariant ("a skipped service is
#   NOT a satisfied dependency") but no other path could express it.
#
# WHY THIS SHIPPED
#   Skip was covered on plain services. Cron was covered on unskipped units.
#   NO fixture in the suite put `skip` and `cron` on the SAME unit — the defect
#   lived exactly in that seam. This case pins the combination.
#
# EXPECTED OUTCOME
#   - a skipped cron unit never executes, and never accrues an exit code,
#   - it reports `skipped` — not `done` — across cron boundaries,
#   - an unskipped cron unit in the same manifest still fires (control),
#   - dependents of a skipped unit do not run, transitively,
#   - a cascade restart of a skipped root does not resurrect its dependents,
#   - an independent unit is untouched.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
PROJECT=skipcron

rm -f /tmp/cron_fired.log /tmp/live_fired.log

section "cold start"
sysg start --config "$CONFIG" --daemonize >/tmp/start.err 2>&1
check "$?" "start exits 0"

S="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
[ "$(unit_count "$S")" = "8" ]
check "$?" "status reports all 8 units"

section "a skipped cron unit reports skipped, not queued or done"
STATE="$(unit_field "$S" skipped_cron state "$PROJECT")"
echo "  skipped_cron state=$STATE"
[ "$STATE" = "skipped" ]
check "$?" "skipped cron unit is 'skipped' before any boundary"

section "dependents of a skipped unit do not run"
[ "$(pgrep -f 'echo CHILD_LINE' | wc -l | tr -d ' ')" = "0" ]
check "$?" "direct dependent of a skipped unit has no process"
[ "$(pgrep -f 'echo GRANDCHILD_LINE' | wc -l | tr -d ' ')" = "0" ]
check "$?" "transitive dependent of a skipped unit has no process"
[ "$(pgrep -f 'echo INDEPENDENT_LINE' | wc -l | tr -d ' ')" -ge 1 ]
check "$?" "the independent unit is unaffected"

# A minute boundary is where the bug used to fire. Wait past one so the assertion
# below covers a real scheduler tick, not just the pre-boundary state.
section "waiting out a cron boundary (75s)"
sleep 75

section "the skipped cron unit never executed"
[ ! -f /tmp/cron_fired.log ]
check "$?" "skipped cron command never ran"

# Control: proves the expression really did come due inside the wait above, so
# the assertion before it is meaningful rather than vacuous.
[ -f /tmp/live_fired.log ]
check "$?" "control: the unskipped cron unit DID fire in the same window"

section "the skipped cron unit did not fabricate a successful run"
S2="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
STATE2="$(unit_field "$S2" skipped_cron state "$PROJECT")"
echo "  skipped_cron state after boundary=$STATE2"
[ "$STATE2" = "skipped" ]
check "$?" "skipped cron unit is STILL 'skipped' after a boundary (not 'done')"

EXIT_AFTER="$(unit_field "$S2" skipped_cron last_exit "$PROJECT")"
echo "  skipped_cron last_exit=$EXIT_AFTER"
[ "$EXIT_AFTER" = "None" ] || [ "$EXIT_AFTER" = "absent" ]
check "$?" "skipped cron unit records no exit code for a run that never happened"

HEALTH="$(unit_field "$S2" skipped_cron health "$PROJECT")"
echo "  skipped_cron health=$HEALTH"
[ "$HEALTH" != "healthy" ]
check "$?" "a unit that never ran is not reported healthy on a fake success"

section "a cascade restart of a skipped root does not resurrect dependents"
sysg restart -s cascade_root -p "$PROJECT" --config "$CONFIG" >/tmp/cascade.err 2>&1
echo "  restart rc=$?"
sleep 3
[ "$(pgrep -f 'echo ROOT_LINE' | wc -l | tr -d ' ')" = "0" ]
check "$?" "skipped root still has no process after a cascade restart"
[ "$(pgrep -f 'echo CHILD_LINE' | wc -l | tr -d ' ')" = "0" ]
check "$?" "cascade restart did not start the dependent of a skipped root"
[ "$(pgrep -f 'echo GRANDCHILD_LINE' | wc -l | tr -d ' ')" = "0" ]
check "$?" "cascade restart did not start the transitive dependent"

section "restarting a dependent DIRECTLY still respects a skipped ancestor"
# `restart -s cascade_child` starts its cascade AT the child, so a unit skipped
# ABOVE it is never visited. Without seeding the skip set from config, the child
# restarts happily behind a dependency that never came up.
sysg restart -s cascade_child -p "$PROJECT" --config "$CONFIG" >/tmp/direct.err 2>&1
echo "  restart rc=$?"
sleep 3
[ "$(pgrep -f 'echo CHILD_LINE' | wc -l | tr -d ' ')" = "0" ]
check "$?" "direct restart of a dependent did not start it behind a skipped ancestor"

section "a CONDITIONALLY skipped ancestor blocks a directly-restarted dependent"
# `skip: <command>` leaves no flag in the manifest, so the cascade can only know
# the verdict from the lifecycle the last evaluation recorded. Seeding from the
# manifest alone let this dependent restart behind a skipped dependency.
[ "$(pgrep -f 'echo COND_CHILD_LINE' | wc -l | tr -d ' ')" = "0" ]
check "$?" "dependent of a conditionally skipped unit did not run at boot"

sysg restart -s cond_child -p "$PROJECT" --config "$CONFIG" >/tmp/cond.err 2>&1
echo "  restart rc=$?"
sleep 3
[ "$(pgrep -f 'echo COND_CHILD_LINE' | wc -l | tr -d ' ')" = "0" ]
check "$?" "direct restart did not start it behind a conditionally skipped dep"

section "a unit skipped while ALREADY RUNNING is stopped, not left behind"
# The cascade marks a newly-skipped dependent as skipped. If it was running at
# that moment its process must be stopped too — a unit recorded as skipped while
# its process keeps running is an untracked process and a lying status.
[ "$(unit_field "$S2" independent state "$PROJECT")" = "running" ]
check "$?" "precondition: the independent unit is running before we skip it"

python3 - <<'PY'
src = open('/usecase/stack.yaml').read()
old = "  independent:\n    command: sh -c 'while true; do echo INDEPENDENT_LINE; sleep 1; done'\n    restart_policy: always"
new = "  independent:\n    skip: true\n    command: sh -c 'while true; do echo INDEPENDENT_LINE; sleep 1; done'\n    restart_policy: always"
assert old in src, "independent block not found — fixture and injection drifted"
open('/tmp/skipped.yaml', 'w').write(src.replace(old, new))
PY

sysg restart -s independent -p "$PROJECT" --config /tmp/skipped.yaml >/tmp/skipnow.err 2>&1
echo "  restart rc=$?"
sleep 3
[ "$(pgrep -f 'echo INDEPENDENT_LINE' | wc -l | tr -d ' ')" = "0" ]
check "$?" "a running unit that becomes skipped has its process stopped"

S4="$(sysg status --config /tmp/skipped.yaml --format json 2>/dev/null)"
IND_STATE="$(unit_field "$S4" independent state "$PROJECT")"
echo "  independent state=$IND_STATE"
[ "$IND_STATE" = "skipped" ]
check "$?" "status agrees the newly skipped unit is skipped"

sysg stop --supervisor >/dev/null 2>&1
finish
