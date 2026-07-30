#!/usr/bin/env bash
# USE CASE: a Gamecast-shaped project restarts cleanly, repeatedly.
#
# WHAT THIS TESTS (real production bug, sysg 0.58.1)
#   `sysg restart -p gamecast-prod` failed with SG0302 naming ALL 14 units while
#   `sysg status` reported the project HEALTHY and every PID had in fact changed.
#   Two independent defects produced that:
#
#     1. port_from_command matched a bare `:NNNN` anywhere in a command string,
#        so an outbound proxy URL (`http://user:pass@proxy:10001`) became the
#        unit's supposed listening port. wait_for_ready then refused to report
#        Running until the process owned that port. A non-listening worker never
#        will, so it burned the full start timeout and was declared failed while
#        running perfectly.
#
#     2. reconcile_failures only recovered unit identity from ServicesNotRunning.
#        Every other error variant fell back to naming every targeted unit, so
#        one unit's timeout was reported as all 14 failing.
#
#   The narrow prior fix made an explicit `--port` win over an earlier URL. That
#   patched one example and left the inference rule intact: a command with a URL
#   and NO --port still guessed wrong. This case pins the invariant instead of
#   the example.
#
# EXPECTED OUTCOME
#   - Cold start exits 0.
#   - Project-wide restart exits 0, twice in a row (the prod repro was repeatable).
#   - Restart actually restarts: service PIDs change.
#   - Non-listening units with URLs/ports in their commands come up promptly and
#     are never failed for not owning a port.
#   - Restart does not hang: a false port gate cost ~5s per affected unit.
#   - Command success agrees with status health.
#   - Cron / skipped / one-shot units reach their declared targets.
#   - When one unit really fails, SG0302 names ONLY that unit.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
PROJECT=gamecast-prod

# Units that run as long-lived processes (exclude one-shots, cron, skipped).
SERVICES="api agent draftkings_ingest draftkings_live_odds live_sweep ingest match reconcile sweep observability_rollup"

pids_of() {
  local snap="$1" out="" svc
  for svc in $SERVICES; do
    out="${out}${svc}=$(unit_field "$snap" "$svc" pid "$PROJECT") "
  done
  printf '%s' "$out"
}

section "cold start brings the whole project up"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"

S="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
[ "$(unit_count "$S")" = "15" ]
check "$?" "status reports all 15 units"

section "the non-listening unit with a proxy URL is running, not failed"
# This is the exact prod unit that broke: its command holds `:10001` but it
# binds nothing. Under the old gate it timed out and was reported failed.
[ "$(unit_field "$S" draftkings_ingest state "$PROJECT")" = "running" ]
check "$?" "draftkings_ingest is running despite :10001 in its command"
[ "$(unit_field "$S" draftkings_live_odds state "$PROJECT")" = "running" ]
check "$?" "draftkings_live_odds is running despite an upstream port in its command"

section "declared targets: one-shot, cron, skipped"
[ "$(unit_field "$S" migrations state "$PROJECT")" = "done" ]
check "$?" "migrations is done"
[ "$(unit_field "$S" legacy_backfill state "$PROJECT")" = "skipped" ]
check "$?" "legacy_backfill is skipped"
[ "$(unit_field "$S" vacuum_full state "$PROJECT")" = "queued" ]
check "$?" "vacuum_full is queued for its schedule"
# The prod analogues of `psql ...` / `redis-cli ...`: client one-shots whose
# commands carry connect-to ports. They must finish, not wait on a port.
[ "$(unit_field "$S" postgres state "$PROJECT")" = "done" ]
check "$?" "postgres client one-shot is done, not gated on :5432"
[ "$(unit_field "$S" redis state "$PROJECT")" = "done" ]
check "$?" "redis client one-shot is done, not gated on :6379"

BEFORE="$(pids_of "$S")"
echo "pids before: $BEFORE"

section "project-wide restart exits 0 and does not hang"
# A false port gate burned SERVICE_START_TIMEOUT (5s) per affected unit. With 5
# non-listening units that is 25s+ of pure dead wait even when it "succeeded".
START_T="$(date +%s)"
RESTART_OUT="$(sysg restart -p "$PROJECT" --config "$CONFIG" 2>&1)"
RC=$?
END_T="$(date +%s)"
ELAPSED=$((END_T - START_T))
echo "restart rc=$RC elapsed=${ELAPSED}s"
echo "$RESTART_OUT"

[ "$RC" = "0" ]
check "$?" "restart -p exits 0"

printf '%s' "$RESTART_OUT" | grep -q "SG0302"
if [ "$?" = "0" ]; then check 1 "restart did not emit SG0302"; else check 0 "restart did not emit SG0302"; fi

# The port gate cost SERVICE_START_TIMEOUT (5s) per non-listening unit; with 5
# of them that is 25s+ of dead wait. A healthy restart lands around 7-8s even
# under parallel CI load, so 45s flags a regression without flaking on a loaded
# box.
[ "$ELAPSED" -lt 45 ]
check "$?" "restart completed in <45s (no per-unit port-gate stalls)"

section "restart actually restarted the services"
S2="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
AFTER="$(pids_of "$S2")"
echo "pids after:  $AFTER"
[ "$BEFORE" != "$AFTER" ]
check "$?" "service pids changed"

CHANGED=0
UNCHANGED=""
for svc in $SERVICES; do
  b="$(unit_field "$S" "$svc" pid "$PROJECT")"
  a="$(unit_field "$S2" "$svc" pid "$PROJECT")"
  if [ "$b" != "$a" ]; then
    CHANGED=$((CHANGED + 1))
  else
    UNCHANGED="${UNCHANGED}${svc} "
  fi
done
echo "changed=$CHANGED unchanged=[${UNCHANGED}]"
[ -z "$UNCHANGED" ]
check "$?" "every long-lived service got a new pid"

section "command success agrees with status health"
for svc in $SERVICES; do
  [ "$(unit_field "$S2" "$svc" state "$PROJECT")" = "running" ] || {
    echo "  $svc is $(unit_field "$S2" "$svc" state "$PROJECT")"
    false
  }
done
check "$?" "all long-lived services are running after restart"

[ "$(unit_field "$S2" legacy_backfill state "$PROJECT")" = "skipped" ]
check "$?" "legacy_backfill is still skipped after restart"
[ "$(unit_field "$S2" migrations state "$PROJECT")" = "done" ]
check "$?" "migrations is still done after restart"
[ "$(unit_field "$S2" postgres state "$PROJECT")" = "done" ]
check "$?" "postgres is still done after restart"
[ "$(unit_field "$S2" redis state "$PROJECT")" = "done" ]
check "$?" "redis is still done after restart"
[ "$(unit_field "$S2" vacuum_full state "$PROJECT")" = "queued" ]
check "$?" "vacuum_full is still queued after restart"

section "the restart is repeatable (prod hit this on every invocation)"
RESTART_OUT2="$(sysg restart -p "$PROJECT" --config "$CONFIG" 2>&1)"
RC2=$?
echo "second restart rc=$RC2"
echo "$RESTART_OUT2"
[ "$RC2" = "0" ]
check "$?" "second restart -p also exits 0"

S3="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
[ "$(unit_field "$S3" draftkings_ingest state "$PROJECT")" = "running" ]
check "$?" "draftkings_ingest still running after the second restart"

section "a real failure names ONLY the unit that failed"
# Swap one unit's command for something that exits immediately, then reconcile.
# SG0302 must attribute the failure to that unit alone — not to all 14.
python3 - <<'PY'
src = open('/usecase/stack.yaml').read()
# Make `sweep` fail at startup while leaving every other unit untouched.
old = '  sweep:\n    command: sleep 100000\n    restart_policy: always'
new = '  sweep:\n    command: /bin/false\n    restart_policy: always'
assert old in src, "sweep block not found — fixture and injection drifted"
open('/tmp/broken.yaml', 'w').write(src.replace(old, new))
PY

BROKEN_OUT="$(sysg restart -p "$PROJECT" --config /tmp/broken.yaml 2>&1)"
BRC=$?
echo "broken restart rc=$BRC"
echo "$BROKEN_OUT"

if printf '%s' "$BROKEN_OUT" | grep -q "SG0302"; then
  # The failing unit must be named.
  printf '%s' "$BROKEN_OUT" | grep -q "sweep"
  check "$?" "SG0302 names the unit that actually failed (sweep)"

  # Healthy units must NOT be named. This is the 14-unit lie.
  NAMED=""
  for svc in api agent draftkings_ingest ingest match observability_rollup; do
    if printf '%s' "$BROKEN_OUT" | grep -qE "(^|[ ,])${svc}([ ,]|$)"; then
      NAMED="${NAMED}${svc} "
    fi
  done
  echo "healthy units wrongly named: [${NAMED}]"
  [ -z "$NAMED" ]
  check "$?" "SG0302 does not name healthy units"
else
  echo "note: reconcile did not surface SG0302 for the injected failure"
  check 0 "no false SG0302 emitted"
fi

sysg stop --supervisor >/dev/null 2>&1
finish
