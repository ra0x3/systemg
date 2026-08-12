#!/usr/bin/env bash
# Kernel-enforced sandbox UAT (Phase 3). Boots a schema-v3 manifest whose
# service is Landlock-confined to /allowed (+ system paths) and must be denied
# access to /secret. Also verifies fail-closed: a v3 service requesting an
# unenforceable key (seccomp) refuses to start.
#
# Requires a kernel with Landlock (>= 5.13) and a privileged container.
set -u
PASS=0; FAIL=0
check() { if [ "$1" = "0" ]; then PASS=$((PASS+1)); echo "PASS: $2"; else FAIL=$((FAIL+1)); echo "FAIL: $2"; fi; }

# Detect Landlock support: landlock_create_ruleset (syscall 444) with a probe.
LANDLOCK_OK=0
python3 - 2>/dev/null <<'PY' && LANDLOCK_OK=1
import ctypes
libc = ctypes.CDLL("libc.so.6", use_errno=True)
class attr(ctypes.Structure):
    _fields_=[("fs",ctypes.c_uint64),("net",ctypes.c_uint64),("scoped",ctypes.c_uint64)]
a=attr(1,0,0)
fd=libc.syscall(444, ctypes.byref(a), ctypes.sizeof(a), 0)
import sys; sys.exit(0 if fd>=0 else 1)
PY

sysg --sys start -c /etc/systemg/systemg.yaml --daemonize >/dev/null 2>&1
sleep 4
LOG="$(sysg --sys logs --service confined --no-follow -c /etc/systemg/systemg.yaml 2>/dev/null)"
echo "--- service log (landlock_ok=$LANDLOCK_OK) ---"; echo "$LOG"

if [ "$LANDLOCK_OK" = "1" ]; then
  echo "$LOG" | grep -q "ok"
  check "$?" "confined service can read its allowed path"
  echo "$LOG" | grep -q "DENIED"
  check "$?" "confined service is DENIED access to /secret (landlock enforced)"
  ! echo "$LOG" | grep -q "LEAK"
  check "$?" "no leak: /secret/deny.txt was never read"
else
  # No Landlock: fail-closed must refuse the confined service, never run it
  # unprotected. Assert via service state, not a command-line grep (the
  # entrypoint script text would false-match). The confined unit must NOT be
  # running, and no sleep payload from it may exist.
  ST="$(sysg --sys status --service confined -c /etc/systemg/systemg.yaml --format json 2>/dev/null)"
  ! echo "$ST" | grep -qi '"state":"running"'
  check "$?" "no landlock: confined service is not running (refused)"
fi

sysg --sys stop -c /etc/systemg/systemg.yaml >/dev/null 2>&1
sysg --sys purge >/dev/null 2>&1

# fail-closed: a v3 service with an unenforceable seccomp key must refuse.
cat > /tmp/failclosed.yaml <<EOF
version: "3"
projects:
  fc:
    name: FC
    services:
      blocked:
        command: "sleep 300"
        isolation:
          seccomp: "baseline"
EOF
OUT="$(sysg --sys start -c /tmp/failclosed.yaml --daemonize 2>&1)"
sleep 2
STATUS="$(sysg --sys status --service blocked -c /tmp/failclosed.yaml --format json 2>&1)"
echo "$OUT $STATUS" | grep -qiE "isolation.seccomp|cannot enforce|not startable|SG0"
check "$?" "v3 unenforceable seccomp refuses the service (fail-closed)"
! echo "$STATUS" | grep -qi '"state":"running"'
check "$?" "blocked service is not running (no unprotected process)"
sysg --sys purge >/dev/null 2>&1

echo "sandbox: ${PASS} passed, ${FAIL} failed"
[ "${FAIL}" = "0" ]
