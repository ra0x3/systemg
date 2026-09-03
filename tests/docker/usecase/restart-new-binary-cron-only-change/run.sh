#!/usr/bin/env bash
# USE CASE: a deploy ships a new binary and edits only cron units; restart must
# still bounce the long-running service.
#
# WHAT THIS TESTS (real production bug, sysg 0.66.9, arbitration)
#   The deploy installed a new arb-rs at the same path, then ran
#   `sysg restart --config sysg.prod.yaml`. The manifest change in that deploy
#   added seven cron units and removed one; no long-running unit's definition
#   moved. The supervisor logged "Restarting 7 of 27 services" then "Restarted 0
#   service(s); 7 passed over" — every target was cron-managed, so cron skipped
#   them all — and the command exited 0. The API kept its old process, and the
#   deploy's health gate failed for 30 polls on a stale git sha.
#
#   Two defects made that green:
#     1. `restart` derived its scope from the manifest diff, so a diff that
#        touched only cron units targeted no long-running unit at all. A rebuilt
#        binary at an unchanged path hashes identically and is invisible there.
#     2. SG0304 (restart bounced nothing) exempted any run that REMOVED a unit —
#        "stopping a removed unit counts as work" — and the deploy removed one
#        cron unit, which disarmed the only guard.
#
#   `server` re-reads /usecase/build into /usecase/serving on every start, which
#   is the "new binary" stand-in: if the process was never bounced, `serving`
#   still holds the old build.
#
# EXPECTED OUTCOME
#   - Rewriting the build, adding a cron unit AND removing one, then plain
#     `restart -c`: exits 0, server has a NEW pid, and serving == v2.
#   - The same shape under `--delta`: bounces nothing, so it must FAIL with
#     SG0304 rather than exit 0 over a stale process.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

echo v1 > /usecase/build

section "boot the stack on build v1"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 3
S1="$(sysg status --format json 2>/dev/null)"
SERVER_1="$(unit_field "$S1" server pid demo)"
echo "before -> server:$SERVER_1 serving:$(cat /usecase/serving 2>/dev/null)"
[ "$(cat /usecase/serving 2>/dev/null)" = "v1" ]
check "$?" "server is serving build v1"

section "ship build v2, add one cron unit and remove another, restart -c"
echo v2 > /usecase/build
cat > "$CONFIG" <<'EOF'
version: "2"
projects:
  demo:
    name: Demo
    services:
      server:
        command: "sh -c 'cat /usecase/build > /usecase/serving && exec sleep 3000'"
        restart_policy: "always"
      brief_check:
        command: "sh -c 'echo brief >> /usecase/cron-runs'"
        cron:
          expression: "15 4 * * *"
EOF
sysg restart --config "$CONFIG" >/tmp/r.out 2>/tmp/r.err
RC=$?
cat /tmp/r.out /tmp/r.err | tail -20
[ "$RC" = "0" ]
check "$?" "restart -c exits 0"
sleep 3
S2="$(sysg status --format json 2>/dev/null)"
SERVER_2="$(unit_field "$S2" server pid demo)"
echo "after  -> server:$SERVER_2 serving:$(cat /usecase/serving 2>/dev/null)"
[ "$SERVER_2" != "$SERVER_1" ] && pid_alive "$SERVER_2"
check "$?" "server pid CHANGED (the cron-only diff did not narrow the restart)"
[ "$(cat /usecase/serving 2>/dev/null)" = "v2" ]
check "$?" "server is serving build v2 (the new binary actually took)"

section "the same shape under --delta must fail loudly, not silently"
echo v3 > /usecase/build
cat > "$CONFIG" <<'EOF'
version: "2"
projects:
  demo:
    name: Demo
    services:
      server:
        command: "sh -c 'cat /usecase/build > /usecase/serving && exec sleep 3000'"
        restart_policy: "always"
      prune_brief:
        command: "sh -c 'echo prune >> /usecase/cron-runs'"
        cron:
          expression: "15 5 * * *"
EOF
sysg restart --config "$CONFIG" --delta >/tmp/d.out 2>/tmp/d.err
RC=$?
cat /tmp/d.out /tmp/d.err | tail -20
[ "$RC" != "0" ]
check "$?" "restart --delta that bounced nothing exits non-zero"
stderr_has_code SG0304 /tmp/d.err
check "$?" "stderr names SG0304 (restart touched nothing)"
sleep 2
S3="$(sysg status --format json 2>/dev/null)"
[ "$(unit_field "$S3" server pid demo)" = "$SERVER_2" ]
check "$?" "server pid UNCHANGED, which is what SG0304 just reported"
[ "$(cat /usecase/serving 2>/dev/null)" = "v2" ]
check "$?" "server still serving v2, so the failure told the truth"

sysg stop --supervisor >/dev/null 2>&1
finish
