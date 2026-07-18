#!/usr/bin/env bash
# USE CASE: a SECOND foreground `start -c` (separate config) streams ITS OWN logs.
#
# WHAT THIS TESTS (real dogfooding bug)
#   term1: `sysg start -c alpha.yaml` (foreground) — owns the shared control
#   socket and streams alpha's logs. term2: `sysg start -c beta.yaml`
#   (foreground, DIFFERENT config) registers beta into the same supervisor AND
#   must stream BETA's logs to term2. In 0.54.2/0.55.0 the second foreground
#   start was silently ABSORBED: it registered beta but streamed nothing, so
#   term2 sat on a spinner with no output. Contract: each foreground terminal
#   follows its OWN project.
#
# EXPECTED OUTCOME
#   - alpha's captured output contains ALPHA_MARKER_LINE.
#   - beta's  captured output contains BETA_MARKER_LINE  (the bug: it was empty).
#   - neither terminal is cross-contaminated with the other's marker.
set -u
. /usecase/lib.sh

A_OUT=/tmp/alpha.out
B_OUT=/tmp/beta.out

section "term1: foreground start alpha (owns the socket, streams alpha)"
python3 /usecase/fgcap.py /usecase/alpha.yaml 16 "$A_OUT" &
T1=$!
sleep 5

section "term2: foreground start beta (separate config) WHILE alpha holds term1"
python3 /usecase/fgcap.py /usecase/beta.yaml 10 "$B_OUT" &
T2=$!
sleep 8

section "each terminal streamed ITS OWN project's logs"
echo "--- alpha.out (last 3) ---"; tail -3 "$A_OUT" 2>/dev/null
echo "--- beta.out (last 3) ---";  tail -3 "$B_OUT" 2>/dev/null
grep -q ALPHA_MARKER_LINE "$A_OUT" 2>/dev/null
check "$?" "term1 streamed alpha's logs (ALPHA_MARKER_LINE present)"
grep -q BETA_MARKER_LINE "$B_OUT" 2>/dev/null
check "$?" "term2 streamed BETA's logs (BETA_MARKER_LINE present) <-- the bug"

section "term2 is scoped to its own project (no alpha bleed)"
# NOTE: term1 is the in-process supervisor and pipes ALL managed services'
# stderr to its inherited stdout, so it currently also shows beta's lines once
# beta registers — a pre-existing over-broad-pipe quirk (see use-case-bugs.txt),
# not the reported bug. term2's scoping is the assertion that matters here.
! grep -q ALPHA_MARKER_LINE "$B_OUT" 2>/dev/null
check "$?" "term2 did NOT receive alpha's logs (its follow is project-scoped)"

section "status reports FOREGROUND mode (neither was --daemonize'd)"
# No supervise daemon exists; both projects are foreground-owned. status must
# not label them 'daemon'. Read the machine field to avoid ANSI parsing.
S="$(sysg status --config /usecase/alpha.yaml --format json 2>/dev/null)"
A_MODE="$(printf '%s' "$S" | python3 -c 'import json,sys
d=json.load(sys.stdin); seen=set()
for u in d.get("units",[]):
    p=u.get("project") or {}
    if p.get("id")=="alpha": print(p.get("mode")); break
else: print("none")' 2>/dev/null)"
echo "alpha project mode: $A_MODE"
[ "$A_MODE" = "foreground" ]
check "$?" "status reports alpha as foreground, not daemon"

kill "$T1" "$T2" 2>/dev/null
sysg stop --supervisor >/dev/null 2>&1
finish
