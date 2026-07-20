#!/usr/bin/env bash
# USE CASE: `restart -s <A>` cascades to A's transitive dependents.
#
# WHAT THIS TESTS
#   If B depends on A and A is restarted, B must be restarted too — B needs to
#   re-handshake the fresh A; a stale B pointing at the old A is a leak. So
#   restarting `db` must bounce db AND everything that (transitively) depends on
#   it: api (dep db), report (dep api), worker (dep db). An independent service
#   `lonely` (depends on nothing) must be untouched — the cascade follows the
#   dependency graph, it does not bounce the whole project.
#
#   Graph:  db <- api <- report      db <- worker      lonely (unrelated)
#
# EXPECTED OUTCOME
#   - restart -s db: db, api, report, worker ALL get NEW live pids.
#   - lonely keeps its ORIGINAL pid (not a dependent of db).
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot both projects"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3

S="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
DB0="$(unit_field "$S" db pid shop)"
API0="$(unit_field "$S" api pid shop)"
REP0="$(unit_field "$S" report pid shop)"
WK0="$(unit_field "$S" worker pid shop)"
LON0="$(unit_field "$S" lonely pid shop)"
echo "before: db=$DB0 api=$API0 report=$REP0 worker=$WK0 lonely=$LON0"
for p in "$DB0" "$API0" "$REP0" "$WK0" "$LON0"; do
  [ -n "$p" ] && [ "$p" != "absent" ] && pid_alive "$p" || { check 1 "all five services up before restart"; break; }
done
check "$?" "all five services up on live pids before restart"

section "restart -s db cascades to db + all transitive dependents"
sysg restart --config "$CONFIG" -p shop -s db
check "$?" "restart -s db exits 0"
sleep 3

S2="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
DB1="$(unit_field "$S2" db pid shop)"
API1="$(unit_field "$S2" api pid shop)"
REP1="$(unit_field "$S2" report pid shop)"
WK1="$(unit_field "$S2" worker pid shop)"
LON1="$(unit_field "$S2" lonely pid shop)"
echo "after:  db=$DB1 api=$API1 report=$REP1 worker=$WK1 lonely=$LON1"

new_and_alive() { [ -n "$2" ] && [ "$2" != "absent" ] && [ "$2" != "$1" ] && pid_alive "$2"; }

new_and_alive "$DB0" "$DB1";   check "$?" "db restarted on a NEW live pid (was $DB0, now $DB1)"
new_and_alive "$API0" "$API1"; check "$?" "api (dep db) cascaded to a NEW live pid (was $API0, now $API1)"
new_and_alive "$REP0" "$REP1"; check "$?" "report (dep api, transitive) cascaded to a NEW live pid (was $REP0, now $REP1)"
new_and_alive "$WK0" "$WK1";   check "$?" "worker (dep db, sibling branch) cascaded to a NEW live pid (was $WK0, now $WK1)"

section "the independent (non-dependent) service is untouched"
[ "$LON1" = "$LON0" ] && pid_alive "$LON1"
check "$?" "lonely keeps its ORIGINAL pid (not a db dependent, no cascade)"

sysg stop --supervisor >/dev/null 2>&1
finish
