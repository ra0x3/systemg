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

# --- status JSON helpers ---------------------------------------------------
# All take a status-json blob on stdin-equivalent as $1.

# unit_field <json> <unit-name> <field> [project-id]
# Prints the field of the unit named <unit-name> (optionally scoped to a
# project), "absent" if no such unit, "noparse" if the blob is not JSON.
# Nested objects (project, process) print their obvious identity: project.id,
# process.pid.
unit_field() {
  printf '%s' "$1" | python3 -c '
import json,sys
name,field=sys.argv[1],sys.argv[2]
proj=sys.argv[3] if len(sys.argv)>3 and sys.argv[3] else None
try: data=json.load(sys.stdin)
except Exception: print("noparse"); sys.exit()
for u in data.get("units",[]):
    if u.get("name")!=name: continue
    if proj is not None and (u.get("project") or {}).get("id")!=proj: continue
    v=u.get(field)
    if field=="pid": v=(u.get("process") or {}).get("pid")
    if isinstance(v,dict): v=v.get("id","?")
    print(v); break
else: print("absent")
' "$2" "$3" "${4:-}"
}

# unit_count <json> -> number of units in the snapshot (-1 if unparseable).
unit_count() {
  printf '%s' "$1" | python3 -c '
import json,sys
try: data=json.load(sys.stdin)
except Exception: print(-1); sys.exit()
print(len(data.get("units",[])))
'
}

# pid_alive <pid> -> exit 0 if the pid is a live process.
pid_alive() { kill -0 "$1" 2>/dev/null; }

# stderr_has_code <code> <file> -> exit 0 if the captured stderr names the code.
stderr_has_code() { grep -q "$1" "$2"; }

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
