#!/usr/bin/env bash
# USE CASE: a reconcile where ONE added unit cannot come up fails partial, not
# all-or-nothing — the good services survive and SG0302 names only the bad one.
#
# WHAT THIS TESTS
#   The adversary edits the manifest to add a service whose command dies
#   instantly, while leaving the existing services healthy. The reconcile must:
#     - keep the healthy, UNCHANGED services running (never a blanket teardown),
#     - actually attempt the added unit,
#     - report SG0302 (ReconcileIncomplete) naming ONLY the unit that failed.
#   A supervisor that tears everything down because one unit failed, or that
#   reports success, is broken.
#
# EXPECTED OUTCOME
#   - Boot demo (web, api) healthy; record pids.
#   - Add `bad` (exits immediately) to the manifest; restart -c.
#   - restart exits non-zero with SG0302 naming `bad`.
#   - web and api are STILL running (unchanged units adopted, not bounced).
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot the healthy config"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3
S1="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
WEB1="$(unit_field "$S1" web pid)"
API1="$(unit_field "$S1" api pid)"
echo "before -> web:$WEB1 api:$API1"
[ -n "$WEB1" ] && [ -n "$API1" ] && pid_alive "$WEB1" && pid_alive "$API1"
check "$?" "web and api running before restart"

section "add a service that dies instantly, then restart -c"
cat > "$CONFIG" <<'EOF'
version: "2"
projects:
  demo:
    name: Demo
    services:
      web:
        command: "sleep 3000"
      api:
        command: "sleep 3000"
      bad:
        command: "sh -c 'exit 1'"
EOF
echo "config now adds 'bad' (exits 1 immediately)"

sysg restart --config "$CONFIG" 2>/tmp/r.txt
RC=$?
cat /tmp/r.txt
[ "$RC" != "0" ]
check "$?" "restart with a failing added unit exits non-zero"
stderr_has_code SG0302 /tmp/r.txt
check "$?" "stderr names SG0302 (reconcile incomplete)"
grep -q "bad" /tmp/r.txt
check "$?" "SG0302 names the failed unit 'bad'"

section "the healthy services SURVIVED the partial failure"
sleep 1
pid_alive "$WEB1"
check "$?" "web STILL alive (unchanged unit adopted, not torn down)"
pid_alive "$API1"
check "$?" "api STILL alive (unchanged unit adopted, not torn down)"
S2="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
[ "$(unit_field "$S2" web pid)" = "$WEB1" ]
check "$?" "web pid UNCHANGED (never bounced by the reconcile)"
[ "$(unit_field "$S2" api pid)" = "$API1" ]
check "$?" "api pid UNCHANGED (never bounced by the reconcile)"

sysg stop --supervisor >/dev/null 2>&1
finish
