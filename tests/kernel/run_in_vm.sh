#!/usr/bin/env bash
# Runs the kernel-facing sandbox checks inside a virtme-ng VM booting a specific
# kernel, against target/release/sysg (no Docker). REQUIRE_LANDLOCK gates
# whether Landlock enforcement is asserted (new kernels) or its refusal is
# (old kernels).
set -u
SYSG="target/release/sysg"
PASS=0; FAIL=0
check() { if [ "$1" = "0" ]; then PASS=$((PASS+1)); echo "PASS: $2"; else FAIL=$((FAIL+1)); echo "FAIL: $2"; fi; }

echo "kernel: $(uname -r)  landlock-required: ${REQUIRE_LANDLOCK:-0}"

# procfs / cgroups needed for system mode.
mount -t proc proc /proc 2>/dev/null || true

mkdir -p /allowed /secret /etc/systemg /var/lib/systemg /var/log/systemg
echo ok > /allowed/ok.txt
echo topsecret > /secret/deny.txt

# Probe Landlock.
LANDLOCK_OK=0
python3 - 2>/dev/null <<'PY' && LANDLOCK_OK=1
import ctypes
libc = ctypes.CDLL("libc.so.6", use_errno=True)
class attr(ctypes.Structure):
    _fields_=[("fs",ctypes.c_uint64),("net",ctypes.c_uint64),("scoped",ctypes.c_uint64)]
a=attr(1,0,0)
import sys; sys.exit(0 if libc.syscall(444, ctypes.byref(a), ctypes.sizeof(a), 0)>=0 else 1)
PY
echo "landlock_ok=$LANDLOCK_OK"

if [ "${REQUIRE_LANDLOCK:-0}" = "1" ] && [ "$LANDLOCK_OK" != "1" ]; then
  echo "FAIL: REQUIRE_LANDLOCK set but kernel $(uname -r) lacks Landlock"
  exit 1
fi

# seccomp baseline-v1: allowed runs, chmod blocked.
cat > /etc/systemg/seccomp.yaml <<EOF
version: "3"
services:
  s:
    command: "sh -c 'echo hello && (chmod 777 /allowed/ok.txt 2>/dev/null && echo CHMOD_OK || echo CHMOD_BLOCKED); sleep 60'"
    isolation:
      seccomp: "baseline-v1"
EOF
"$SYSG" --sys start -c /etc/systemg/seccomp.yaml --daemonize >/dev/null 2>&1
sleep 3
SLOG="$("$SYSG" --sys logs --service s --no-follow -c /etc/systemg/seccomp.yaml 2>/dev/null)"
echo "$SLOG" | grep -q hello && echo "$SLOG" | grep -q CHMOD_BLOCKED
check "$?" "seccomp baseline-v1 blocks chmod on kernel $(uname -r)"
"$SYSG" --sys stop -c /etc/systemg/seccomp.yaml >/dev/null 2>&1
"$SYSG" --sys purge >/dev/null 2>&1

# Landlock: confine to /allowed; /secret must be denied.
cat > /etc/systemg/landlock.yaml <<EOF
version: "3"
services:
  c:
    command: "sh -c 'cat /allowed/ok.txt && (cat /secret/deny.txt 2>/dev/null && echo LEAK || echo DENIED); sleep 60'"
    isolation:
      landlock:
        ro_paths: ["/allowed", "/bin", "/lib", "/usr"]
EOF
"$SYSG" --sys start -c /etc/systemg/landlock.yaml --daemonize >/dev/null 2>&1
sleep 3
CLOG="$("$SYSG" --sys logs --service c --no-follow -c /etc/systemg/landlock.yaml 2>/dev/null)"
CST="$("$SYSG" --sys status --service c -c /etc/systemg/landlock.yaml --format json 2>/dev/null)"
if [ "$LANDLOCK_OK" = "1" ]; then
  echo "$CLOG" | grep -q ok && echo "$CLOG" | grep -q DENIED && ! echo "$CLOG" | grep -q LEAK
  check "$?" "landlock confines to /allowed, denies /secret on kernel $(uname -r)"
else
  ! echo "$CST" | grep -qi '"state":"running"'
  check "$?" "no landlock: confined service refused, not run unconfined ($(uname -r))"
fi
"$SYSG" --sys stop -c /etc/systemg/landlock.yaml >/dev/null 2>&1
"$SYSG" --sys purge >/dev/null 2>&1

# Oracle must be clean throughout.
"$SYSG" --sys doctor >/dev/null 2>&1
check "$?" "invariant oracle clean on kernel $(uname -r)"

echo "kernel-matrix: ${PASS} passed, ${FAIL} failed"
[ "${FAIL}" = "0" ]
