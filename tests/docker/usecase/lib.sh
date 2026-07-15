# Shared helpers for use-case tests. Source this at the top of each case's run.sh:
#   . /usecase/lib.sh
# Then use section/check/finish. `check` takes a status (0 = pass) and a label.
export HOME=/root

PASS=0
FAIL=0

section() { printf '\n========== %s ==========\n' "$1"; }

check() {
  if [ "$1" = "0" ]; then
    echo "PASS: $2"
    PASS=$((PASS + 1))
  else
    echo "FAIL: $2"
    FAIL=$((FAIL + 1))
  fi
}

# Asserts that running "$@" exits 0.
check_ok() {
  local label="$1"
  shift
  "$@" >/dev/null 2>&1
  check "$?" "$label"
}

# Asserts that running "$@" exits non-zero.
check_fails() {
  local label="$1"
  shift
  if "$@" >/dev/null 2>&1; then
    check 1 "$label"
  else
    check 0 "$label"
  fi
}

finish() {
  section "RESULT"
  echo "passed: $PASS  failed: $FAIL"
  if [ "$FAIL" -eq 0 ]; then
    echo "=> GREEN"
    exit 0
  fi
  echo "=> RED"
  exit 1
}
