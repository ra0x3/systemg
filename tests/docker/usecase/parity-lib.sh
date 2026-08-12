# Shared helpers for parity-* cases: run the same scenario in user mode and
# system mode (--sys), capture a canonical shape per lane, then require the
# shapes to be identical. The user-mode lane runs as a real non-root user
# (`parity`, created on first use); the sys lane runs as root with --sys.

SYSG_MODE_FLAGS=""

parity_user_ensure() {
  id parity >/dev/null 2>&1 && return 0
  useradd -m -s /bin/bash parity 2>/dev/null || adduser -D -s /bin/bash parity
}

mode_begin() {
  MODE="$1"
  case "$MODE" in
    user)
      SYSG_MODE_FLAGS=""
      parity_user_ensure
      chmod 644 "$CONFIG"
      ;;
    sys) SYSG_MODE_FLAGS="--sys" ;;
  esac
  section "lane: $MODE mode"
}

msysg() {
  if [ "$MODE" = "user" ]; then
    su parity -s /bin/bash -c "sysg $(printf '%q ' "$@")"
  else
    sysg $SYSG_MODE_FLAGS "$@"
  fi
}

mode_end() {
  SUP_PID=""
  for PF in /var/lib/systemg/sysg.pid /home/parity/.local/share/systemg/sysg.pid; do
    [ -f "$PF" ] && SUP_PID="$(tr -cd '0-9' < "$PF")"
  done
  msysg stop --config "$CONFIG" >/dev/null 2>&1
  sleep 2
  msysg purge >/dev/null 2>&1
  check "$?" "[$MODE] purge exits 0"
  msysg status --config "$CONFIG" >/dev/null 2>/tmp/postpurge.err
  grep -qiE "SG0206|no supervisor" /tmp/postpurge.err
  check "$?" "[$MODE] no supervisor answers after purge"
  if [ -n "$SUP_PID" ]; then
    ! kill -0 "$SUP_PID" 2>/dev/null
    check "$?" "[$MODE] supervisor pid $SUP_PID is dead after purge"
  fi
  pkill -x sleep 2>/dev/null
  sleep 1
}

# After stop+purge, the invariant oracle must report a clean or empty world:
# no lingering Running service, no live recorded pid. This is the empty-
# teardown invariant, checked without a bespoke per-case assertion.
teardown_oracle_ok() {
  msysg doctor 2>/tmp/td-oracle.err
  RC=$?
  [ "$RC" = "0" ]
  if [ "$RC" != "0" ]; then cat /tmp/td-oracle.err; fi
  check "$?" "[$MODE] oracle: world is clean after teardown"
}

# Runs the invariant oracle and fails the case on any error-severity finding.
# Call this whenever the world is expected to be fully consistent (all declared
# services up, or fully torn down) — it catches the whole "status lied" /
# ghost-state class without a bespoke assertion per scenario.
oracle_ok() {
  msysg doctor --format json > /tmp/oracle.json 2>/tmp/oracle.err
  RC=$?
  if [ "$RC" != "0" ]; then
    echo "--- doctor findings ---"; cat /tmp/oracle.json /tmp/oracle.err
  fi
  [ "$RC" = "0" ]
  check "$?" "[$MODE] invariant oracle: world is consistent"
}

shape_units() {
  python3 - "$1" <<'PY'
import json,sys
data=json.loads(open(sys.argv[1]).read() or "{}")
units=sorted((u.get("name",""),u.get("state","").lower()) for u in data.get("units",[]))
for name,state in units: print(f"{name}:{state}")
PY
}

shapes_match() {
  diff /tmp/shape.user /tmp/shape.sys >/tmp/shape.diff 2>&1
  RC=$?
  [ "$RC" != "0" ] && cat /tmp/shape.diff
  return $RC
}
