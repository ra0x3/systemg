#!/usr/bin/env bash
# USE CASE: `-p <loaded-project>` is authoritative for logs — sysg already knows
# that project's config, so it must NOT reject with SG0201.
#
# WHAT THIS TESTS (real dogfooding bug; the status TUI 'L' key hit this)
#   Two SEPARATE config files (one.yaml, two.yaml) are started into ONE resident
#   supervisor. `two` is a loaded project. `sysg logs -p two -s twosvc` names a
#   project the supervisor already has registered with its own config — it must
#   read two's logs, NOT fail SG0201 "project two does not match the resolved
#   config" (which resolved one.yaml/the primary). Even the TUI's own combo
#   (-c one.yaml -p two) must resolve to the loaded project, not reject.
#
# EXPECTED OUTCOME
#   - logs -p two -s twosvc            -> streams TWO_LOG_LINE (no SG0201)
#   - logs -c one.yaml -p two -s twosvc-> streams TWO_LOG_LINE (loaded -p wins)
#   - logs -p one -s onesvc            -> streams ONE_LOG_LINE (primary still ok)
set -u
. /usecase/lib.sh

section "start two separate configs into one supervisor"
sysg start -c /usecase/one.yaml --daemonize
check "$?" "start one.yaml exits 0 (boots supervisor)"
sysg start -c /usecase/two.yaml --daemonize
check "$?" "start two.yaml exits 0 (registers 'two' into the supervisor)"
sleep 4
sysg status 2>/dev/null | grep -qiE 'Project: One' && sysg status 2>/dev/null | grep -qiE 'Project: Two'
check "$?" "both projects are loaded in the supervisor"

section "logs -p two -s twosvc (loaded non-primary, NO -c) must NOT SG0201"
OUT="$(sysg logs -p two -s twosvc --lines 5 2>&1)"
echo "$OUT" | grep -q SG0201 && echo "GOT SG0201 (bug)"
! echo "$OUT" | grep -q SG0201
check "$?" "no SG0201 for a loaded project id"
echo "$OUT" | grep -q TWO_LOG_LINE
check "$?" "streamed TWO's logs (TWO_LOG_LINE present)"

section "the TUI's combo: -c one.yaml -p two must resolve to loaded 'two'"
OUT2="$(sysg logs -c /usecase/one.yaml -p two -s twosvc --lines 5 2>&1)"
! echo "$OUT2" | grep -q SG0201
check "$?" "loaded -p 'two' wins over the -c one.yaml config (no SG0201)"
echo "$OUT2" | grep -q TWO_LOG_LINE
check "$?" "streamed TWO's logs via the TUI-style combo"

section "the primary project still works too"
OUT3="$(sysg logs -p one -s onesvc --lines 5 2>&1)"
echo "$OUT3" | grep -q ONE_LOG_LINE
check "$?" "logs -p one -s onesvc streams ONE's logs"

sysg stop --supervisor >/dev/null 2>&1
finish
