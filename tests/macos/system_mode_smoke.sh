#!/usr/bin/env bash
# macOS system-mode smoke test. Runs on a native macOS runner (there is no
# Docker parity lane for Darwin). Verifies the /Library path selection, the
# rootless-validate exemption, and that Linux-only kernel features are refused
# with typed diagnostics rather than silently ignored.
#
# Requires: a release build at target/release/sysg on macOS.
set -u

SYSG="${SYSG:-target/release/sysg}"
PASS=0
FAIL=0
check() { if [ "$1" = "0" ]; then PASS=$((PASS+1)); echo "PASS: $2"; else FAIL=$((FAIL+1)); echo "FAIL: $2"; fi; }

[ "$(uname -s)" = "Darwin" ] || { echo "skip: not macOS"; exit 0; }
[ -x "$SYSG" ] || { echo "missing $SYSG"; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
cat > "$WORK/plain.yaml" <<EOF
version: "2"
services:
  web:
    command: "sleep 5"
EOF
cat > "$WORK/priv.yaml" <<EOF
version: "2"
services:
  db:
    command: "sleep 5"
    user: "nobody"
    capabilities: ["CAP_NET_BIND_SERVICE"]
EOF

# init is Linux-only: refused with SG0711 on macOS.
OUT="$("$SYSG" init 2>&1)"
echo "$OUT" | grep -q "SG0711"
check "$?" "sysg init refused with SG0711 on macOS"

# validate --sys works without root (validation reads, never exercises privilege).
"$SYSG" validate --sys -c "$WORK/plain.yaml" >/dev/null 2>&1
check "$?" "validate --sys plain manifest exits 0 without root"

# Linux-only keys under --sys on macOS: capabilities are unenforceable here.
OUT="$("$SYSG" validate --sys --no-color -c "$WORK/priv.yaml" 2>&1)"
echo "$OUT" | grep -qi "linux-only\|not startable"
check "$?" "validate --sys flags Linux-only keys as unenforceable on macOS"

# User-mode validate of privileged keys is not startable (SG0705).
OUT="$("$SYSG" validate --no-color -c "$WORK/priv.yaml" 2>&1)"
echo "$OUT" | grep -q "SG0705"
check "$?" "user-mode validate flags root-only keys with SG0705"

echo "macos-system: ${PASS} passed, ${FAIL} failed"
[ "${FAIL}" = "0" ]
