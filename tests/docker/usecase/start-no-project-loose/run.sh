#!/usr/bin/env bash
# USE CASE: start a project-less (loose) config.
#
# WHAT THIS TESTS
#   `sysg start -c stack.yaml --daemonize` on a project-less (loose) config:
#   top-level `services:` with no `project:`/`projects:` key. Loose services
#   must persist under projects/__loose__/, not a legacy-<hash> dir.
#
# EXPECTED OUTCOME
#   - start exits 0.
#   - `job` is running with a live pid.
#   - on-disk state lives at projects/__loose__/pid.xml (NOT projects/legacy-*/).
#   NOTE: this case is expected RED until the loose-id fix aligns the start path
#   with StateStore::loose().
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
STATE_DIR="$HOME/.local/share/systemg"

section "start the project-less (loose) config"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3

STATUS="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
PID="$(unit_field "$STATUS" job pid)"
echo "job pid per status: $PID"

section "status reports job running with a live pid"
[ "$(unit_field "$STATUS" job state)" = "running" ]
check "$?" "job is running"
[ -n "$PID" ] && [ "$PID" != "absent" ] && [ "$PID" != "None" ]
check "$?" "job has a pid in status"
pid_alive "$PID"
check "$?" "that pid is actually alive per ps"

section "loose state persists under projects/__loose__/"
[ -f "$STATE_DIR/projects/__loose__/pid.xml" ]
check "$?" "projects/__loose__/pid.xml exists"
! ls -d "$STATE_DIR"/projects/legacy-* 2>/dev/null
check "$?" "no projects/legacy-* dir exists"

sysg stop --config "$CONFIG" >/dev/null 2>&1
finish
