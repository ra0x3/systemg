#!/usr/bin/env bash
# USE CASE: status reports RSS in the right UNITS (bytes), agreeing with the OS.
#
# WHAT THIS TESTS
#   A prod red herring: sysg showed a 66MB API as "20.5GB" RSS — a ~1024x unit
#   error (sysinfo's Process::memory() returns BYTES since v0.30, but the metrics
#   collector still multiplied by 1024). This asserts sysg's reported
#   latest_rss_bytes is within a small factor of the OS's `ps rss` (KB*1024), so
#   a units regression can never silently return.
#
# HARD INVARIANTS
#   - the service allocates ~60MB, so ps rss is tens of MB,
#   - sysg's latest_rss_bytes is within 4x of the OS bytes (catches a 1024x bug),
#   - sysg does NOT report multi-GB for a tens-of-MB process.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot a service with a known ~60MB allocation"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 5

section "compare sysg RSS to the OS RSS"
PID="$(sysg status --config "$CONFIG" --format json 2>/dev/null \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["units"][0]["process"]["pid"])')"
echo "service pid: $PID"
OS_KB="$(ps -o rss= -p "$PID" | tr -d ' ')"
OS_BYTES=$((OS_KB * 1024))
echo "OS ps rss: ${OS_KB}KB = ${OS_BYTES} bytes"

SYSG_BYTES="$(sysg status --config "$CONFIG" --live --format json 2>/dev/null \
  | python3 -c 'import json,sys
d=json.load(sys.stdin)
m=(d["units"][0].get("metrics") or {})
print(m.get("latest_rss_bytes", 0))')"
echo "sysg latest_rss_bytes: ${SYSG_BYTES}"

[ "$SYSG_BYTES" -gt 0 ]
check "$?" "sysg reported a non-zero RSS"

# within 4x either way — generous, but a 1024x unit bug blows straight through it
python3 -c "
import sys
osb=$OS_BYTES; sg=$SYSG_BYTES
if osb==0 or sg==0: sys.exit(1)
ratio = sg/osb if sg>=osb else osb/sg
print(f'ratio sysg/os = {sg/osb:.2f}')
sys.exit(0 if ratio <= 4 else 1)
"
check "$?" "sysg RSS agrees with OS RSS within 4x (no 1024x unit error)"

# a tens-of-MB process must never be reported as multiple GB
[ "$SYSG_BYTES" -lt 2000000000 ]
check "$?" "sysg does not report multi-GB for a ~60MB process"

sysg stop --supervisor >/dev/null 2>&1
finish
