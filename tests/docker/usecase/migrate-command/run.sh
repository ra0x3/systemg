#!/usr/bin/env bash
# USE CASE: `sysg migrate` converts an old-shape (project: + services:) manifest
# into the canonical `projects:` shape, printed to stdout (non-destructive).
# The converted output must itself be a valid manifest that boots to the same
# project id and services.
set -u
. /usecase/lib.sh

LEGACY=/usecase/legacy.yaml
CONVERTED=/usecase/converted.yaml

section "migrate prints converted YAML without touching the source"
sysg migrate "$LEGACY" > "$CONVERTED" 2>/tmp/migrate.err
MIGRATE_RC=$?
echo "migrate rc: $MIGRATE_RC"
cat "$CONVERTED"
[ "$MIGRATE_RC" -eq 0 ]
check "$?" "sysg migrate exits 0"

# The source file must be untouched (still the old shape).
grep -q "^project:" "$LEGACY"
check "$?" "source file left unmodified (still old shape)"

section "converted output is the new projects: shape"
grep -q "^projects:" "$CONVERTED"
check "$?" "converted output uses the projects: key"
grep -qE "^  shop:" "$CONVERTED"
check "$?" "project id 'shop' became a projects: entry"
! grep -q "^project:" "$CONVERTED"
check "$?" "converted output has no legacy top-level project: key"

section "converted manifest boots to the same project + services"
sysg start --config "$CONVERTED" --daemonize
echo "start rc: $?"
sleep 3
STATUS="$(sysg status --config "$CONVERTED" --format json 2>/dev/null)"
printf '%s' "$STATUS" | python3 -c '
import json,sys
data=json.load(sys.stdin)
names={u.get("name") for u in data.get("units",[])}
projs={ (u.get("project") or {}).get("id") for u in data.get("units",[]) }
sys.exit(0 if {"api","worker"}<=names and "shop" in projs else 1)
'
check "$?" "converted manifest runs api+worker under project shop"

sysg stop --config "$CONVERTED" >/dev/null 2>&1
finish
