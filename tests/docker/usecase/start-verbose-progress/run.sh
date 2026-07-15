#!/usr/bin/env bash
# USE CASE: `sysg start --verbose` prints live per-service progress.
#
# WHAT THIS TESTS
#   With --verbose, start must print a line PER SERVICE as it comes up
#   ("Starting alpha...", "Starting beta...") to the user's terminal, driven by
#   progress streamed back from the supervisor -- irrespective of the supervisor
#   log file. Without --verbose, start stays quiet (just the spinner). This is
#   the feature requested for the rebuild.
#
# EXPECTED OUTCOME
#   - `sysg start --verbose -c stack.yaml --daemonize` emits, on its own output,
#     a per-service progress line naming BOTH alpha and beta as they start.
#   - The output references each service name ("alpha" and "beta") in a
#     Starting/started context, not just a single generic "Starting" spinner.
#   Expected RED until the boot-progress streaming + CLI verbose renderer lands.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "verbose start prints per-service progress"
# Capture combined stdout+stderr so we see whatever the CLI renders live.
sysg start --verbose --config "$CONFIG" --daemonize >/tmp/verbose_out.txt 2>&1
check "$?" "verbose start exits 0"
echo "----- captured start output -----"
cat /tmp/verbose_out.txt
echo "---------------------------------"

grep -qiE "start(ing|ed).*alpha|alpha.*start(ing|ed)" /tmp/verbose_out.txt
check "$?" "verbose output names 'alpha' in a start context"
grep -qiE "start(ing|ed).*beta|beta.*start(ing|ed)" /tmp/verbose_out.txt
check "$?" "verbose output names 'beta' in a start context"

sleep 2

section "without --verbose, start does not print per-service lines"
sysg stop --supervisor >/dev/null 2>&1
sleep 1
sysg start --config "$CONFIG" --daemonize >/tmp/quiet_out.txt 2>&1
check "$?" "quiet start exits 0"
if grep -qiE "start(ing|ed).*(alpha|beta)" /tmp/quiet_out.txt; then
  check 1 "quiet start did NOT print per-service lines"
else
  check 0 "quiet start did NOT print per-service lines"
fi

sysg stop --supervisor >/dev/null 2>&1
finish
