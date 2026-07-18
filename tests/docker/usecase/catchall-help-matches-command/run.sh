#!/usr/bin/env bash
# USE CASE: a generic (catch-all) failure points at the FAILING command's help,
# not a fixed `sysg logs` suggestion.
#
# WHAT THIS TESTS (real dogfooding bug)
#   `sysg status` with no supervisor fell through to the generic SG0001 handler,
#   which hardcoded `help: supervisor logs  sysg logs`. Telling someone who ran
#   `status` to go check logs is nonsensical. The catch-all must tailor its help
#   to whichever command was run.
#
# EXPECTED OUTCOME
#   - `sysg status` (no supervisor) help mentions `sysg status --help`,
#     and does NOT suggest `sysg logs`.
set -u
. /usecase/lib.sh

section "status with no supervisor points at status help, not logs"
OUT="$(sysg status 2>&1)"
echo "$OUT" | grep -vE 'WARN'
echo "$OUT" | grep -q 'sysg status --help'
check "$?" "status failure suggests 'sysg status --help'"
! echo "$OUT" | grep -qE 'sysg logs'
check "$?" "status failure does NOT suggest 'sysg logs'"

finish
