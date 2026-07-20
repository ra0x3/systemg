#!/usr/bin/env bash
# USE CASE: the stack SELF-HEALS after a dependency crashes.
#
# WHAT THIS TESTS
#   When a dependency (db) crashes, sysg stops its dependents as casualties
#   (web dep db; report dep db+cache). Once db is respawned and healthy, sysg
#   must AUTO-RESTART those dependents so the whole stack recovers on its own —
#   a dependent must re-handshake the fresh db, and an operator should not have
#   to intervene. Gates proven here:
#     - web (dep db) revives on a NEW pid once db is back.
#     - report (dep db AND cache) revives too — cache stayed up, db recovered,
#       so all its deps are healthy.
#     - audit (dep db, skip:true) STAYS down — skip is honored, no revival.
#     - the crashed db itself is respawned (real supervision).
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
pid_of() { unit_field "$1" "$2" pid shop; }
is_up()   { [ -n "$1" ] && [ "$1" != "absent" ] && [ "$1" != "None" ] && pid_alive "$1"; }
is_down() { [ -z "$1" ] || [ "$1" = "absent" ] || [ "$1" = "None" ] || ! pid_alive "$1"; }

section "boot the stack"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 4
S="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
DB0="$(pid_of "$S" db)"; CACHE0="$(pid_of "$S" cache)"; WEB0="$(pid_of "$S" web)"; REP0="$(pid_of "$S" report)"; AUD0="$(pid_of "$S" audit)"
echo "boot: db=$DB0 cache=$CACHE0 web=$WEB0 report=$REP0 audit=$AUD0"
is_up "$DB0" && is_up "$CACHE0" && is_up "$WEB0" && is_up "$REP0"
check "$?" "db, cache, web, report are up"
is_down "$AUD0"
check "$?" "audit is skipped (skip:true dependent stays down)"

section "kill db — dependents fall, then the stack self-heals"
kill -9 "$DB0" 2>/dev/null
echo "killed db $DB0; sysg should respawn db and revive web + report"

DB_NEW=""; WEB_NEW=""; REP_NEW=""; i=0
while [ "$i" -lt 40 ]; do
  sleep 1
  S="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
  DB_NEW="$(pid_of "$S" db)"; WEB_NEW="$(pid_of "$S" web)"; REP_NEW="$(pid_of "$S" report)"
  if is_up "$DB_NEW" && [ "$DB_NEW" != "$DB0" ] \
     && is_up "$WEB_NEW" && [ "$WEB_NEW" != "$WEB0" ] \
     && is_up "$REP_NEW" && [ "$REP_NEW" != "$REP0" ]; then
    echo "healed at ~${i}s: db=$DB_NEW web=$WEB_NEW report=$REP_NEW"
    break
  fi
  i=$((i+1))
done

is_up "$DB_NEW" && [ "$DB_NEW" != "$DB0" ]
check "$?" "db was respawned on a NEW pid (was $DB0, now $DB_NEW)"
is_up "$WEB_NEW" && [ "$WEB_NEW" != "$WEB0" ]
check "$?" "web (dep db) self-healed on a NEW pid (was $WEB0, now $WEB_NEW)"
is_up "$REP_NEW" && [ "$REP_NEW" != "$REP0" ]
check "$?" "report (dep db+cache) self-healed on a NEW pid (was $REP0, now $REP_NEW)"

section "invariants after self-heal"
S="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
CACHE1="$(pid_of "$S" cache)"
[ "$CACHE1" = "$CACHE0" ] && is_up "$CACHE1"
check "$?" "cache (never a casualty) kept its pid"
is_down "$(pid_of "$S" audit)"
check "$?" "audit stayed down (skip honored through the crash/heal)"

sysg stop --supervisor >/dev/null 2>&1
finish
