#!/usr/bin/env bash
# USE CASE: a key systemg does not know is refused, not ignored.
#
# WHAT THIS TESTS (real production report, sysg 0.66.1)
#   A downstream stack declared `on_error: scripts/send-discord.py <unit>` on
#   twelve services. It is not a systemg key — service hooks are `hooks:` with
#   `onstart:`/`onerr:` beneath — so serde dropped it silently. `sysg validate`
#   reported zero problems on a manifest whose alerting was entirely dead, and
#   nothing in the logs ever mentioned the key again. Schema conformance could
#   only be checked by reading systemg's source.
#
# EXPECTED OUTCOME
#   - validate FAILS on the unknown key and names it,
#   - the diagnostic points at the documented spelling,
#   - start refuses the same manifest (validate must refuse what start refuses),
#   - the corrected manifest validates clean — the check is about unknown keys,
#     not about hooks being unwelcome.
set -u
. /usecase/lib.sh

section "an unknown key fails validation"
sysg validate -c /usecase/stack.yaml >/tmp/validate.out 2>&1
RC=$?
cat /tmp/validate.out
[ "$RC" != "0" ]
check "$?" "validate exits non-zero on an unknown service key"

grep -q "unknown-field" /tmp/validate.out
check "$?" "the diagnostic is classified as an unknown field"

grep -q "on_error" /tmp/validate.out
check "$?" "the offending key is named"

grep -q "onerr" /tmp/validate.out
check "$?" "the fix points at the documented hook spelling"

section "start refuses what validate refuses"
sysg start -c /usecase/stack.yaml --daemonize >/tmp/start.out 2>&1
RC=$?
cat /tmp/start.out
[ "$RC" != "0" ]
check "$?" "start refuses the manifest too"

grep -q "on_error" /tmp/start.out
check "$?" "start's refusal names the key as well"

section "the documented spelling is accepted"
sysg validate -c /usecase/good.yaml >/tmp/good.out 2>&1
check "$?" "the corrected manifest validates clean"

finish
