#!/usr/bin/env bash
# USE CASE: a GNARLY internal restart journey — skip/unskip, transitive deps,
# a changed command rippling a subtree, cascade, and a remove — all against ONE
# long-lived supervisor, with ps and status forced to agree at every step.
#
# Graph (v1):  db <- migrate(skip) <- api      db <- worker      lonely
#
#   1. boot v1: db/worker/lonely up, migrate SKIPPED — and api SKIPPED too,
#      because a skipped dependency propagates skip to its dependents.
#   2. UNSKIP migrate + change db's command (V1->V2), restart -c: migrate now
#      runs, db bounced to V2, dependents bounced, ordering db -> migrate -> api.
#   3. restart -s db (no -c): cascades to db + migrate + api + worker (new pids);
#      lonely (no dep on db) untouched.
#   4. RE-SKIP migrate + REMOVE worker, restart -c: migrate and its dependent
#      api stop (skipped), worker is verifiably DEAD, and db/lonely survive.
set -u
. /usecase/lib.sh

V1=/usecase/stack.v1.yaml
V2=/usecase/stack.v2.yaml
V3=/usecase/stack.v3.yaml
CONFIG=/usecase/stack.yaml
cp "$V1" "$CONFIG"

pid_of() { unit_field "$1" "$2" pid shop; }
is_up()  { local p="$1"; [ -n "$p" ] && [ "$p" != "absent" ] && [ "$p" != "None" ] && pid_alive "$p"; }
is_down() { local p="$1"; [ -z "$p" ] || [ "$p" = "absent" ] || [ "$p" = "None" ] || ! pid_alive "$p"; }

# ============================================================================
section "1. boot v1 — migrate skipped; api (dep migrate) is skipped too"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3
S="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
DB0="$(pid_of "$S" db)"; API0="$(pid_of "$S" api)"; WK0="$(pid_of "$S" worker)"; LON0="$(pid_of "$S" lonely)"; MIG0="$(pid_of "$S" migrate)"
echo "v1: db=$DB0 migrate=$MIG0 api=$API0 worker=$WK0 lonely=$LON0"
is_up "$DB0" && is_up "$WK0" && is_up "$LON0"
check "$?" "db, worker, lonely are up"
is_down "$MIG0"
check "$?" "migrate is SKIPPED (not running) per skip:true"
is_down "$API0"
check "$?" "api is SKIPPED too — a skipped dependency blocks its dependent"
[ "$(cat /tmp/db.marker 2>/dev/null)" = "V1" ]
check "$?" "db ran its V1 command (marker=V1)"

# ============================================================================
section "2. unskip migrate + change db command (V1->V2), restart -c"
cp "$V2" "$CONFIG"
sysg restart --config "$CONFIG"
check "$?" "full restart -c exits 0"
# api depends on migrate; it only launches once migrate is ready, so give the
# dependency chain time to settle (db -> migrate -> api).
sleep 9
S="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
MIG1="$(pid_of "$S" migrate)"; DB1="$(pid_of "$S" db)"; API1="$(pid_of "$S" api)"
echo "v2: db=$DB1 migrate=$MIG1 api=$API1"
is_up "$MIG1"
check "$?" "migrate is now RUNNING (unskipped)"
[ "$(cat /tmp/db.marker 2>/dev/null)" = "V2" ]
check "$?" "db bounced onto its V2 command (marker flipped to V2)"
is_up "$DB1" && is_up "$API1"
check "$?" "db and api are up on the new manifest (api followed its now-live dep)"

# ============================================================================
section "3. restart -s db (no -c) cascades to db + transitive dependents"
S="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
DBb="$(pid_of "$S" db)"; MIGb="$(pid_of "$S" migrate)"; APIb="$(pid_of "$S" api)"; WKb="$(pid_of "$S" worker)"; LONb="$(pid_of "$S" lonely)"
sysg restart --service db
check "$?" "restart -s db exits 0"
sleep 3
S="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
DBc="$(pid_of "$S" db)"; MIGc="$(pid_of "$S" migrate)"; APIc="$(pid_of "$S" api)"; WKc="$(pid_of "$S" worker)"; LONc="$(pid_of "$S" lonely)"
echo "cascade: db $DBb->$DBc migrate $MIGb->$MIGc api $APIb->$APIc worker $WKb->$WKc lonely $LONb->$LONc"
changed_up() { [ "$2" != "$1" ] && is_up "$2"; }
changed_up "$DBb" "$DBc";   check "$?" "db got a new pid (cascade root)"
changed_up "$MIGb" "$MIGc"; check "$?" "migrate (dep db) cascaded to a new pid"
changed_up "$APIb" "$APIc"; check "$?" "api (dep migrate, transitive) cascaded to a new pid"
changed_up "$WKb" "$WKc";   check "$?" "worker (dep db) cascaded to a new pid"
[ "$LONc" = "$LONb" ] && is_up "$LONc"
check "$?" "lonely (independent) kept its pid — cascade followed the graph"

# ============================================================================
section "4. re-skip migrate + remove worker, restart -c"
WK_BEFORE="$(pid_of "$S" worker)"
cp "$V3" "$CONFIG"
sysg restart --config "$CONFIG"
check "$?" "full restart -c exits 0"
sleep 4
S="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
MIG3="$(pid_of "$S" migrate)"; DB3="$(pid_of "$S" db)"; API3="$(pid_of "$S" api)"; LON3="$(pid_of "$S" lonely)"
echo "v3: db=$DB3 migrate=$MIG3 api=$API3 lonely=$LON3 (worker removed, was $WK_BEFORE)"
is_down "$MIG3"
check "$?" "migrate is SKIPPED again (re-skip honored on restart)"
is_down "$WK_BEFORE"
check "$?" "the removed worker's old process is verifiably DEAD"
[ "$(pid_of "$S" worker)" = "absent" ]
check "$?" "status no longer lists worker (reconcile-removed)"
is_up "$DB3" && is_down "$API3" && is_up "$LON3"
check "$?" "db and lonely survive while api follows its skipped dependency"

sysg stop --supervisor >/dev/null 2>&1
finish
