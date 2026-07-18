#!/usr/bin/env bash
# USE CASE: `status --format json` always emits JSON, including on empty results.
#
# WHAT THIS TESTS (real dogfooding bug)
#   The empty-result check ran BEFORE the machine-format branch, so a filter that
#   matched nothing printed the prose sentence "No matching units found." on
#   STDOUT even under `--format json`. Any consumer parsing sysg's output — a
#   script, an agent, the matrix harness that found this — crashes on an empty
#   result set instead of reading `"units": []`. A machine format that is only
#   sometimes machine-readable is worse than none: it fails at the exact moment
#   the caller is handling "nothing is running".
#
# EXPECTED OUTCOME
#   - a matching filter yields parseable JSON with units (baseline);
#   - a NON-matching filter yields parseable JSON with an EMPTY units array;
#   - the prose sentence never appears on stdout under --format json;
#   - the human (non-json) path still prints prose.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "start a project so there is something to filter"
sysg start --config "$CONFIG" --daemonize >/dev/null 2>&1
check "$?" "project started"
sleep 4

section "baseline: a MATCHING json query parses and has units"
sysg status --format json >/tmp/j_match.out 2>/dev/null
python3 -c "
import json,sys
d=json.load(open('/tmp/j_match.out'))
sys.exit(0 if len(d.get('units',[])) > 0 else 1)"
check "$?" "matching --format json parses and reports units"

section "the regression: a NON-matching json query must still be JSON"
sysg status -s definitely_not_a_service --format json >/tmp/j_empty.out 2>/dev/null
echo "--- raw output ---"; cat /tmp/j_empty.out

python3 -c "import json; json.load(open('/tmp/j_empty.out'))" 2>/dev/null
check "$?" "empty --format json is PARSEABLE JSON (was prose)"

python3 -c "
import json,sys
d=json.load(open('/tmp/j_empty.out'))
sys.exit(0 if d.get('units') == [] else 1)" 2>/dev/null
check "$?" "empty result is an empty units array"

! grep -q "No matching units found" /tmp/j_empty.out
check "$?" "the prose sentence is absent from json output"

section "the human path still speaks prose"
sysg status -s definitely_not_a_service >/tmp/j_human.out 2>&1
grep -q "No matching units found" /tmp/j_human.out
check "$?" "non-json output still prints the readable message"

sysg stop --supervisor >/dev/null 2>&1
finish
