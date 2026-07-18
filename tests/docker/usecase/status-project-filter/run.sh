#!/usr/bin/env bash
# USE CASE: status -p and -s filters scope the output correctly across projects.
#
# WHAT THIS TESTS
#   Two projects declare the SAME service names (alpha/web, beta/web, ...). status
#   filtering must scope precisely:
#     - `status -p alpha`        -> only alpha's units, never beta's.
#     - `status -p alpha -s web` -> exactly alpha/web, not beta/web.
#     - unfiltered status         -> all four units.
#   A filter that leaks a sibling project's rows, or collapses same-named services
#   across projects, is a scoping bug.
#
# EXPECTED OUTCOME
#   - Boot alpha (web, worker) + beta (web, worker) = 4 units.
#   - status (no filter) shows 4 units.
#   - status -p alpha shows exactly alpha's 2 units.
#   - status -p alpha -s web shows exactly 1 unit (alpha/web) on alpha's pid.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot both projects"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3

section "unfiltered status shows all 4 units"
SALL="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
[ "$(unit_count "$SALL")" = "4" ]
check "$?" "status lists all 4 units"
ALPHA_WEB="$(unit_field "$SALL" web pid alpha)"
BETA_WEB="$(unit_field "$SALL" web pid beta)"
echo "alpha/web:$ALPHA_WEB beta/web:$BETA_WEB"
[ -n "$ALPHA_WEB" ] && [ -n "$BETA_WEB" ] && [ "$ALPHA_WEB" != "$BETA_WEB" ]
check "$?" "alpha/web and beta/web are distinct units"

section "status -p alpha scopes to alpha only"
SA="$(sysg status --config "$CONFIG" --project alpha --format json 2>/dev/null)"
[ "$(unit_count "$SA")" = "2" ]
check "$?" "status -p alpha shows exactly 2 units"
[ "$(unit_field "$SA" web pid alpha)" = "$ALPHA_WEB" ]
check "$?" "alpha/web present under -p alpha"
[ "$(unit_field "$SA" web pid beta)" = "absent" ]
check "$?" "beta/web NOT leaked into -p alpha"

section "status -p alpha -s web scopes to exactly alpha/web"
SAW="$(sysg status --config "$CONFIG" --project alpha --service web --format json 2>/dev/null)"
[ "$(unit_count "$SAW")" = "1" ]
check "$?" "status -p alpha -s web shows exactly 1 unit"
[ "$(unit_field "$SAW" web pid alpha)" = "$ALPHA_WEB" ]
check "$?" "the single unit is alpha/web on its pid"

sysg stop --supervisor >/dev/null 2>&1
finish
