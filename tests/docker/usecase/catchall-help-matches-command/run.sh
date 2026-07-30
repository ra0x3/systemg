#!/usr/bin/env bash
# USE CASE: a failure's help points at the FAILING command, never a fixed
# `sysg logs` suggestion — and a known condition carries its own code rather
# than falling through to the catch-all at all.
#
# WHAT THIS TESTS (two real dogfooding bugs, in sequence)
#   1. `sysg status` with no supervisor fell through to the generic SG0001
#      handler, which hardcoded `help: supervisor logs  sysg logs`. Telling
#      someone who ran `status` to go check logs is nonsensical, so the
#      catch-all learned to tailor its help to whichever command was run.
#
#   2. That left the deeper problem: `status` with no supervisor is not a
#      generic failure at all. It reported `SG0001: command failed`, which
#      names no cause and offers no fix. It is a supervisor condition and
#      carries SG0206.
#
#   Fixing (2) removed the only easy way to trigger (1) — so this case now
#   pins the outcome instead of the mechanism: the diagnostic must name the
#   real condition, say how to fix it, and never send a `status` user to logs.
#
# EXPECTED OUTCOME
#   - `sysg status` with no supervisor reports SG0206, not SG0001,
#   - its help says how to start a supervisor,
#   - it does NOT suggest `sysg logs`.
set -u
. /usecase/lib.sh

section "status with no supervisor names the real condition"
OUT="$(sysg status 2>&1)"
echo "$OUT" | grep -vE 'WARN'

echo "$OUT" | grep -q 'SG0206'
check "$?" "status failure is typed SG0206 (the supervisor is the problem)"

! echo "$OUT" | grep -q 'SG0001'
check "$?" "status failure is NOT the generic catch-all"

section "its help is actionable and relevant to what was run"
echo "$OUT" | grep -q 'sysg start --daemonize'
check "$?" "status failure says how to start a supervisor"

# The original bug: a `status` failure telling the user to go read logs.
! echo "$OUT" | grep -qE 'sysg logs'
check "$?" "status failure does NOT suggest 'sysg logs'"

finish
