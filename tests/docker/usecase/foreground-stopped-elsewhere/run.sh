#!/usr/bin/env bash
# USE CASE: a foreground `start` YIELDS its terminal when its project is stopped
# from ANOTHER terminal (while the supervisor stays up hosting siblings).
#
# WHAT THIS TESTS (real dogfooding bug)
#   term1: `sysg start -c beta.yaml` (foreground) becomes the supervisor.
#   term2: `sysg start -c alpha.yaml` (foreground) attaches alpha.
#   Then `sysg stop -p alpha` from a THIRD shell removes alpha only; beta (and
#   the supervisor) stay up. term2's foreground start must DETACH and return the
#   terminal — not wedge on the spinner forever. The old wait only woke on Ctrl-C
#   or the whole supervisor dying, so stopping just this project hung the term.
#
# EXPECTED OUTCOME
#   - after `stop -p alpha`, alpha's foreground process EXITS (marker written).
#   - beta's foreground process is still running (its project was untouched).
set -u
. /usecase/lib.sh

BETA_MARK=/tmp/beta.exit
ALPHA_MARK=/tmp/alpha.exit

section "term1: foreground start beta (becomes supervisor)"
python3 /usecase/fgwait.py /usecase/beta.yaml 30 "$BETA_MARK" &
sleep 4

section "term2: foreground start alpha (attaches to the supervisor)"
python3 /usecase/fgwait.py /usecase/alpha.yaml 30 "$ALPHA_MARK" &
sleep 5

sysg status 2>/dev/null | grep -qiE 'Project: Alpha' && sysg status 2>/dev/null | grep -qiE 'Project: Beta'
check "$?" "both alpha and beta are loaded"

section "stop -p alpha from another shell — alpha's foreground must detach"
sysg stop -p alpha
check "$?" "stop -p alpha exits 0"

# Wait for alpha's foreground process to exit (marker appears). If it wedges,
# the marker never shows and this fails — which is the bug.
DETACHED=0
for _ in $(seq 1 12); do
  sleep 1
  [ -f "$ALPHA_MARK" ] && { DETACHED=1; break; }
done
echo "alpha exit marker: $(cat "$ALPHA_MARK" 2>/dev/null || echo '<none — wedged>')"
[ "$DETACHED" = "1" ]
check "$?" "alpha's foreground start YIELDED the terminal after stop -p alpha"

section "beta's foreground is untouched (still running)"
[ ! -f "$BETA_MARK" ]
check "$?" "beta's foreground start did NOT exit (its project was not stopped)"

sysg stop --supervisor >/dev/null 2>&1
finish
