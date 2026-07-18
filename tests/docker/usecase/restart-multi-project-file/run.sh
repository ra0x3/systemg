#!/usr/bin/env bash
# USE CASE: restart a service in a SPECIFIC project of a multi-project FILE.
#
# WHAT THIS TESTS
#   One config file declares TWO projects (shop: db+api, other: svc). Only one
#   becomes the primary; the rest are extra project runtimes. Targeting a
#   service in a NON-primary project — `restart -p shop -s db` — must restart
#   that project's service, NOT fail with "project not managed". 0.54.2 collapsed
#   the multi-project file to one arbitrary project on the -c reload and then
#   looked the wrong project up in the extra map. Both the -p form and the
#   prefixed `-s shop/db` form must resolve to the right project runtime.
#
# EXPECTED OUTCOME
#   - restart -p shop -s db: db (and its dependent api) get NEW live pids.
#   - restart -p other -s svc: svc gets a NEW live pid (the primary path).
#   - the OTHER project is untouched each time.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot the multi-project file"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3
S="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
DB0="$(unit_field "$S" db pid shop)"
API0="$(unit_field "$S" api pid shop)"
SVC0="$(unit_field "$S" svc pid other)"
echo "before: shop/db=$DB0 shop/api=$API0 other/svc=$SVC0"
[ -n "$DB0" ] && [ "$DB0" != "absent" ] && pid_alive "$DB0" && pid_alive "$SVC0"
check "$?" "shop/db and other/svc are up on live pids"

new_alive() { [ -n "$2" ] && [ "$2" != "absent" ] && [ "$2" != "$1" ] && pid_alive "$2"; }

section "restart -p shop -s db (a NON-primary project's service)"
sysg restart --config "$CONFIG" -p shop -s db
check "$?" "restart -p shop -s db exits 0 (no 'project not managed')"
sleep 3
S2="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
DB1="$(unit_field "$S2" db pid shop)"
API1="$(unit_field "$S2" api pid shop)"
SVC1="$(unit_field "$S2" svc pid other)"
echo "after shop restart: shop/db=$DB1 shop/api=$API1 other/svc=$SVC1"
new_alive "$DB0" "$DB1"
check "$?" "shop/db restarted on a NEW live pid (was $DB0, now $DB1)"
new_alive "$API0" "$API1"
check "$?" "shop/api cascaded on the shop project (was $API0, now $API1)"
[ "$SVC1" = "$SVC0" ] && pid_alive "$SVC1"
check "$?" "other/svc untouched by the shop-scoped restart"

section "restart -p other -s svc (the primary project's service)"
sysg restart --config "$CONFIG" -p other -s svc
check "$?" "restart -p other -s svc exits 0"
sleep 3
S3="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
SVC2="$(unit_field "$S3" svc pid other)"
DB2="$(unit_field "$S3" db pid shop)"
echo "after other restart: other/svc=$SVC2 shop/db=$DB2"
new_alive "$SVC1" "$SVC2"
check "$?" "other/svc restarted on a NEW live pid (was $SVC1, now $SVC2)"
[ "$DB2" = "$DB1" ] && pid_alive "$DB2"
check "$?" "shop/db untouched by the other-scoped restart"

section "prefixed selector: restart -s shop/db"
sysg restart --config "$CONFIG" -s shop/db
check "$?" "restart -s shop/db exits 0 (prefixed selector resolves the project)"
sleep 3
S4="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
DB3="$(unit_field "$S4" db pid shop)"
new_alive "$DB2" "$DB3"
check "$?" "shop/db restarted via prefix on a NEW live pid (was $DB2, now $DB3)"

sysg stop --supervisor >/dev/null 2>&1
finish
