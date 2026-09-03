#!/usr/bin/env bash
# USE CASE: a --delta restart that changes only the dependent leaves a completed
# one-shot dependency alone.
#
# WHAT THIS TESTS
#   `probe` is a one-shot that has already completed; `api` depends on it. A
#   caller who edits only api's command and asks for `--delta` is asking for the
#   manifest delta, so probe must NOT be re-run — re-running a finished
#   migration because something downstream changed is how a reconcile becomes
#   dangerous.
#
#   The plain `restart` is a different request: it bounces every declared unit,
#   probe included. `--delta` is the narrow one, and this pins it.
#
# EXPECTED OUTCOME
#   - After restart --delta: probe is still `done` and ran exactly once,
#     api restarted onto V2 with a new pid.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
RUNS=/usecase/probe-runs
VERSION=/usecase/api-version

section "boot with a finite dependency"
sysg start --config "$CONFIG" --daemonize
check "$?" "initial start exits 0"
sleep 3
S1="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
API1="$(unit_field "$S1" api pid demo)"
[ "$(unit_field "$S1" probe state demo)" = "done" ]
check "$?" "probe completed successfully"
[ "$(wc -l < "$RUNS" | tr -d ' ')" = "1" ]
check "$?" "probe ran once"
pid_alive "$API1"
check "$?" "api is running on V1"

section "change only the dependent"
cat > "$CONFIG" <<'EOF'
version: "2"
projects:
  demo:
    name: Demo
    services:
      probe:
        command: "sh -c 'echo probe >> /usecase/probe-runs'"
      api:
        command: "sh -c 'echo V2 > /usecase/api-version && exec sleep 3000'"
        restart_policy: "always"
        depends_on: ["probe"]
EOF
sysg restart --config "$CONFIG" --delta
check "$?" "partial reconcile exits 0"
sleep 3
S2="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
API2="$(unit_field "$S2" api pid demo)"
[ "$(unit_field "$S2" probe state demo)" = "done" ]
check "$?" "completed probe remains done"
[ "$(wc -l < "$RUNS" | tr -d ' ')" = "1" ]
check "$?" "--delta does not rerun a completed one-shot"
[ "$API2" != "$API1" ] && pid_alive "$API2"
check "$?" "api restarted on V2"
[ "$(cat "$VERSION" 2>/dev/null)" = "V2" ]
check "$?" "changed api command ran"

sysg stop --supervisor >/dev/null 2>&1
finish
