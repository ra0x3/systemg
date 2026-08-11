# Shared helpers for parity-* cases: run the same scenario in user mode and
# system mode (--sys), capture a canonical shape per lane, then require the
# shapes to be identical. Containers run as root, so the user-mode lane is
# root-without---sys (SG0701 warns to stderr and proceeds by design).

SYSG_MODE_FLAGS=""

mode_begin() {
  MODE="$1"
  case "$MODE" in
    user) SYSG_MODE_FLAGS="" ;;
    sys) SYSG_MODE_FLAGS="--sys" ;;
  esac
  section "lane: $MODE mode"
}

msysg() { sysg $SYSG_MODE_FLAGS "$@"; }

mode_end() {
  msysg stop --config "$CONFIG" >/dev/null 2>&1
  sleep 2
  msysg purge >/dev/null 2>&1
  check "$?" "[$MODE] purge exits 0"
  msysg status --config "$CONFIG" >/dev/null 2>/tmp/postpurge.err
  grep -qiE "SG0206|no supervisor" /tmp/postpurge.err
  check "$?" "[$MODE] supervisor is down after purge"
  pkill -x sleep 2>/dev/null
  sleep 1
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
