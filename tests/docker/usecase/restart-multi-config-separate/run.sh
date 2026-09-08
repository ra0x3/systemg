#!/usr/bin/env bash
# USE CASE: restarting ONE project whose manifest is its OWN separate file.
#
# WHAT THIS TESTS
#   The real production topology (two projects, two files, one supervisor):
#   alpha.yaml boots the supervisor as the PRIMARY project, beta.yaml is then
#   registered into that same supervisor as an EXTRA project. Restarting either
#   manifest must bounce only that manifest's project and leave the sibling
#   running. `restart -c beta.yaml` is the deploy-script shape: each project's
#   workflow restarts its own file.
#
# EXPECTED OUTCOME
#   - restart -c beta.yaml: beta_svc gets a NEW live pid, alpha_svc pid UNCHANGED.
#   - restart -c alpha.yaml: alpha_svc gets a NEW live pid, beta_svc pid UNCHANGED.
#   - restart -p beta (no -c): beta_svc bounces, alpha untouched, no SG0202.
set -u
. /usecase/lib.sh

section "boot alpha (primary) then register beta from its own file"
sysg start --config /usecase/alpha.yaml --daemonize
check "$?" "start -c alpha.yaml exits 0"
sleep 2
sysg start --config /usecase/beta.yaml --daemonize
check "$?" "start -c beta.yaml exits 0"
sleep 3

S0="$(sysg status --format json 2>/dev/null)"
A0="$(unit_field "$S0" alpha_svc pid alpha)"
B0="$(unit_field "$S0" beta_svc pid beta)"
echo "before -> alpha:$A0 beta:$B0"
[ -n "$A0" ] && [ -n "$B0" ] && [ "$A0" != "$B0" ]
check "$?" "both projects running under one supervisor"

section "restart -c beta.yaml bounces beta, leaves alpha untouched"
sysg restart --config /usecase/beta.yaml 2>/tmp/beta-restart.err
check "$?" "restart -c beta.yaml exits 0"
cat /tmp/beta-restart.err
sleep 3
S1="$(sysg status --format json 2>/dev/null)"
A1="$(unit_field "$S1" alpha_svc pid alpha)"
B1="$(unit_field "$S1" beta_svc pid beta)"
echo "after beta restart -> alpha:$A1 beta:$B1"

[ -n "$B1" ] && [ "$B1" != "$B0" ]
check "$?" "beta_svc bounced (pid changed)"
pid_alive "$B1"
check "$?" "beta_svc new pid alive"
[ "$A1" = "$A0" ]
check "$?" "alpha_svc pid UNCHANGED by restart -c beta.yaml"
pid_alive "$A0"
check "$?" "alpha_svc process still alive"

section "restart -c alpha.yaml bounces alpha, leaves beta untouched"
sysg restart --config /usecase/alpha.yaml 2>/tmp/alpha-restart.err
check "$?" "restart -c alpha.yaml exits 0"
cat /tmp/alpha-restart.err
sleep 3
S2="$(sysg status --format json 2>/dev/null)"
A2="$(unit_field "$S2" alpha_svc pid alpha)"
B2="$(unit_field "$S2" beta_svc pid beta)"
echo "after alpha restart -> alpha:$A2 beta:$B2"

[ -n "$A2" ] && [ "$A2" != "$A1" ]
check "$?" "alpha_svc bounced (pid changed)"
[ "$B2" = "$B1" ]
check "$?" "beta_svc pid UNCHANGED by restart -c alpha.yaml"
pid_alive "$B1"
check "$?" "beta_svc process still alive"

section "restart -p beta with no -c resolves beta's own registered manifest"
cd /usecase
sysg restart --project beta 2>/tmp/beta-p.err
check "$?" "restart -p beta exits 0"
cat /tmp/beta-p.err
sleep 3
S3="$(sysg status --format json 2>/dev/null)"
A3="$(unit_field "$S3" alpha_svc pid alpha)"
B3="$(unit_field "$S3" beta_svc pid beta)"
echo "after restart -p beta -> alpha:$A3 beta:$B3"
[ -n "$B3" ] && [ "$B3" != "$B2" ]
check "$?" "beta_svc bounced by restart -p beta"
[ "$A3" = "$A2" ]
check "$?" "alpha_svc pid UNCHANGED by restart -p beta"

sysg stop --supervisor >/dev/null 2>&1
finish
