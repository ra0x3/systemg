#!/usr/bin/env bash
# USE CASE: start a project-less (loose) config.
#
# WHAT THIS TESTS
#   `sysg start -c stack.yaml --daemonize` on a project-less (loose) config:
#   top-level `services:` with no `project:`/`projects:` key. Such a config is
#   its own project, identified by a slug derived from its manifest path — the
#   shared `__loose__` project is gone, because sharing one slot meant a second
#   loose config evicted the first and killed its service.
#
# EXPECTED OUTCOME
#   - start exits 0.
#   - `job` is running with a live pid.
#   - state lives under projects/<derived-id>/, never projects/__loose__/ or
#     projects/legacy-*/.
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

section "loose state persists under its derived project id"
PROJECT="$(unit_field "$STATUS" job project)"
echo "job project per status: $PROJECT"
[ -n "$PROJECT" ] && [ "$PROJECT" != "absent" ] && [ "$PROJECT" != "None" ]
check "$?" "status reports a derived project id"
[ "$PROJECT" != "__loose__" ]
check "$?" "the derived id is not the legacy shared id"
[ -f "$STATE_DIR/projects/$PROJECT/pid.xml" ]
check "$?" "projects/$PROJECT/pid.xml exists"
[ ! -d "$STATE_DIR/projects/__loose__" ]
check "$?" "no projects/__loose__ dir is created"
! ls -d "$STATE_DIR"/projects/legacy-* 2>/dev/null
check "$?" "no projects/legacy-* dir exists"

sysg stop --config "$CONFIG" >/dev/null 2>&1
finish
