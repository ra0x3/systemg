#!/usr/bin/env bash
# USE CASE: an OLD single-project manifest (top-level `project:` + `services:`)
# must keep working after `projects:` becomes the canonical shape. It boots
# normally and is treated as one project named by project.id. A deprecation
# warning is expected but must not block the boot.
#
# This case must be GREEN both BEFORE and AFTER the schema change -- it is the
# backward-compatibility guard for the migration.
set -u
. /usecase/lib.sh

CONFIG=/usecase/legacy.yaml

unit_field() {
  printf '%s' "$1" | python3 -c '
import json,sys
name,field=sys.argv[1],sys.argv[2]
try: data=json.load(sys.stdin)
except Exception: print("noparse"); sys.exit()
for u in data.get("units",[]):
    if u.get("name")==name:
        v=u.get(field)
        if isinstance(v,dict): v=v.get("id","?")
        print(v); break
else: print("absent")
' "$2" "$3"
}

section "an old-shape project: manifest still boots"
sysg start --config "$CONFIG" --daemonize
echo "start rc: $?"
sleep 3
STATUS="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"

[ "$(unit_field "$STATUS" worker state)" = "running" ]
check "$?" "legacy/worker is running"
[ "$(unit_field "$STATUS" worker project)" = "legacy" ]
check "$?" "worker is grouped under its declared project id 'legacy'"

section "restart -p legacy targets the single project"
sysg restart --project legacy >/dev/null 2>&1
check "$?" "restart -p legacy succeeds on an old-shape file"

sysg stop --config "$CONFIG" >/dev/null 2>&1
finish
