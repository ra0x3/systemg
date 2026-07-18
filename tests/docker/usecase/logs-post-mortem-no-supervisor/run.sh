#!/usr/bin/env bash
# USE CASE: logs stay readable after the supervisor is gone (post-mortem).
#
# WHAT THIS TESTS (real dogfooding bug)
#   Something crashed, the supervisor is down, and you want to know why — the
#   single most common reason to reach for logs. In 0.55.2 `sysg logs -p <proj>`
#   from a directory with no manifest died with SG0203 "could not read a local
#   config file", whose own guidance said "target the project by id with -p
#   instead" — which is EXACTLY what had been passed. Circular, and wrong: the
#   captured logs were sitting on disk the whole time.
#
#   Two separate faults were in play:
#     1. the render fetched a STATUS SNAPSHOT (pure enrichment) and propagated
#        its failure, so no-supervisor + no-config sank the whole command;
#     2. with an empty snapshot the renderers reported "not present in the
#        requested project" / "No active services" instead of reading the files.
#
# EXPECTED OUTCOME
#   With the supervisor stopped and cwd holding NO manifest:
#     - `logs -p <proj> -s <svc>` prints that service's captured lines.
#     - `logs -p <proj>` prints EVERY service the project captured.
#     - neither emits SG0203.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "start the project, let it capture some output"
sysg start --config "$CONFIG" --daemonize >/dev/null 2>&1
check "$?" "project started"
sleep 4

sysg logs -p postmortem -s talker --lines 5 --no-follow 2>/dev/null | grep -q TALKER_MARKER_LINE
check "$?" "logs work WHILE the supervisor is up (baseline)"

section "stop the supervisor entirely"
sysg stop --supervisor >/dev/null 2>&1
sleep 2
! pgrep -x sysg >/dev/null 2>&1
check "$?" "no supervisor is running"

section "post-mortem read from a directory with NO manifest"
mkdir -p /tmp/nomanifest
cd /tmp/nomanifest || exit 1
[ ! -f systemg.yaml ] && [ ! -f sysg.yaml ]
check "$?" "cwd genuinely has no config file"

sysg logs -p postmortem -s talker --lines 5 --no-follow >/tmp/pm_svc.out 2>&1
RC=$?
echo "--- logs -p postmortem -s talker ---"; cat /tmp/pm_svc.out
[ "$RC" = "0" ]
check "$?" "logs -p <proj> -s <svc> exits 0 with no supervisor"
! stderr_has_code SG0203 /tmp/pm_svc.out
check "$?" "no SG0203 config error"
grep -q TALKER_MARKER_LINE /tmp/pm_svc.out
check "$?" "the service's captured lines are shown"

section "the whole project reads back too"
sysg logs -p postmortem --lines 3 --no-follow >/tmp/pm_all.out 2>&1
check "$?" "logs -p <proj> exits 0 with no supervisor"
grep -q TALKER_MARKER_LINE /tmp/pm_all.out
check "$?" "talker's lines present"
grep -q CHATTER_MARKER_LINE /tmp/pm_all.out
check "$?" "chatter's lines present (every captured service, not just one)"
! grep -q "No active services" /tmp/pm_all.out
check "$?" "does not claim there is nothing to show"

finish
