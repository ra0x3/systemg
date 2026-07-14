#!/usr/bin/env bash
# Asserts the terminal dialogue of sysg's rustc-style diagnostics.
#
# Scenario 1 (SG0104): a service crashes at boot with a distinctive message and
# its health check can never pass. The `sysg start` failure must hand the user:
#   * the error code and a one-line statement
#   * the service's OWN last output (the actual crash line)
#   * whether the process is even running
#   * the exact `sysg logs`/`sysg status` commands to dig further
#   * a docs link for the code
#
# Scenario 2 (SG0201): the user passes a -p that does not match the config.
# The error must say which project the config defines, list what the running
# supervisor actually has loaded, and point at `sysg status` + docs.
#
# Rendering rules: ANSI colors on a tty, plain text when piped.
set -u

export HOME=/root

PASS=0
FAIL=0
section() { printf '\n========== %s ==========\n' "$1"; }
check()   { if [ "$1" = "0" ]; then echo "PASS: $2"; PASS=$((PASS+1)); else echo "FAIL: $2"; FAIL=$((FAIL+1)); fi; }
has()     { printf '%s' "$1" | grep -q "$2"; check "$?" "$3"; }

section "boot anchor supervisor"
sysg start --config /repro/anchor.config.yaml --daemonize
sleep 2

section "start crasher project (piped) -> SG0104 diagnostic"
OUT="$(sysg start --config /repro/crasher.config.yaml --daemonize 2>&1)"
RC=$?
printf '%s\n' "$OUT"
[ "$RC" -ne 0 ]; check "$?" "start exits non-zero"

has "$OUT" 'error\[SG0104\]'                          "carries the error code"
has "$OUT" 'failed to become healthy'                 "states what happened"
has "$OUT" 'flux capacitor misaligned'                "quotes the service's own crash output"
has "$OUT" 'the process is not running'               "says whether the process is alive"
has "$OUT" 'sysg logs -s crasher -p diag-repro'       "gives the exact logs command"
has "$OUT" 'sysg status -p diag-repro'                "gives the exact status command"
has "$OUT" 'docs.sysg.dev/errors/SG0104'              "links the docs page for the code"

printf '%s' "$OUT" | grep -q $'\x1b'
[ "$?" -ne 0 ]; check "$?" "piped output contains no ANSI escapes"

section "same failure on a tty -> colored"
TTY_OUT="$(script -qec 'sysg start --config /repro/crasher.config.yaml --daemonize' /dev/null 2>&1)"
printf '%s' "$TTY_OUT" | grep -q $'\x1b\[1;31m'
check "$?" "tty output paints the error header red"

section "service exits immediately -> SG0102 diagnostic"
EOUT="$(sysg start --config /repro/earlyexit.config.yaml --daemonize 2>&1)"
printf '%s\n' "$EOUT"
has "$EOUT" 'error\[SG0102\]'                    "carries the error code"
has "$EOUT" 'exited immediately at start'        "states what happened"
has "$EOUT" 'no such table: users'               "quotes the service's own crash output"
has "$EOUT" 'sysg logs -s earlyexit -p diag-early' "gives the exact logs command"
has "$EOUT" 'docs.sysg.dev/errors/SG0102'        "links the docs page for the code"

section "pre_start fails -> SG0103 diagnostic"
POUT="$(sysg start --config /repro/prefail.config.yaml --daemonize 2>&1)"
printf '%s\n' "$POUT"
has "$POUT" 'error\[SG0103\]'                    "carries the error code"
has "$POUT" 'pre_start for `needs_build` failed' "states what happened"
has "$POUT" 'npm ERR! missing script: build'     "quotes the pre_start output"
has "$POUT" 'services.needs_build.deployment.pre_start' "points at the config key"
has "$POUT" 'docs.sysg.dev/errors/SG0103'        "links the docs page for the code"

section "unstructured failure -> SG0001 catchall"
COUT="$(sysg start --config /repro/does-not-exist.yaml --daemonize 2>&1)"
printf '%s\n' "$COUT"
has "$COUT" 'error\[SG0001\]'                    "carries the catchall code"
has "$COUT" 'supervisor log'                     "points at the supervisor log"
has "$COUT" 'sysg status'                        "suggests checking status"
has "$COUT" 'docs.sysg.dev/errors/SG0001'        "links the docs page for the code"

section "project mismatch -> SG0201 diagnostic"
MOUT="$(sysg logs -s crasher -p wrong-project -c /repro/crasher.config.yaml 2>&1)"
printf '%s\n' "$MOUT"
has "$MOUT" 'error\[SG0201\]'                    "carries the error code"
has "$MOUT" 'diag-repro'                         "names the project the config defines"
has "$MOUT" 'projects loaded'                    "lists what the supervisor has loaded"
has "$MOUT" 'sysg status'                        "suggests listing projects"
has "$MOUT" 'docs.sysg.dev/errors/SG0201'        "links the docs page for the code"

sysg stop >/dev/null 2>&1

section "RESULT"
echo "passed: $PASS  failed: $FAIL"
if [ "$FAIL" -eq 0 ]; then
  echo "=> GREEN: diagnostics render as designed."
  exit 0
else
  echo "=> RED: diagnostic output regressed."
  exit 1
fi
