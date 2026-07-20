#!/usr/bin/env bash
# USE CASE: the supervisor owns its own config (supervisor.xml) with log defaults.
#
# WHAT THIS TESTS
#   The supervisor is impartial infrastructure with a small config of its own —
#   distinct from any project manifest. On first start it writes supervisor.xml
#   at the state dir with sensible defaults (10MB / 5 files). A user can edit it
#   to change the default log-rotation caps for every service that does not set
#   its own; those caps must actually take effect at the log-writer level.
#
# EXPECTED OUTCOME
#   - First start creates supervisor.xml with the default caps (10485760 / 5).
#   - After editing the caps to a TINY max_bytes and restarting, a chatty service
#     with no logs block rotates its log at that tiny cap (proving the supervisor
#     default is honored) — a rotated file appears.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
STATE_DIR="$HOME/.local/share/systemg"
SUP_XML="$STATE_DIR/supervisor.xml"
LOG_DIR="$STATE_DIR/logs/demo"

section "first start writes indented supervisor.xml with operator defaults"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 2
[ -f "$SUP_XML" ]
check "$?" "supervisor.xml was created at the state dir"
echo "--- supervisor.xml ---"; cat "$SUP_XML"; echo
grep -q '10485760' "$SUP_XML"
check "$?" "default max_bytes 10485760 (10MB) present"
grep -q '<max_files>5</max_files>' "$SUP_XML"
check "$?" "default max_files 5 present"
grep -q '^  <logs>$' "$SUP_XML"
check "$?" "supervisor.xml is nested and indented"
grep -q '<pre_start_secs>300</pre_start_secs>' "$SUP_XML"
check "$?" "default pre-start timeout present"
grep -q '<startup_stability_ms>250</startup_stability_ms>' "$SUP_XML"
check "$?" "default startup stability present"
grep -q '<stop_verify_secs>10</stop_verify_secs>' "$SUP_XML"
check "$?" "default stop verification timeout present"

section "edit the cap tiny, restart; the service log rotates at the new cap"
sysg stop --supervisor >/dev/null 2>&1
sleep 1
rm -rf "$LOG_DIR" 2>/dev/null
# 4 KB cap, keep 3 rotated files. A chatty service will blow past 4KB fast, so
# a rotated file (e.g. chatty.log.1 or a numbered variant) must appear.
cat > "$SUP_XML" <<'XML'
<supervisor><logs><max_bytes>4096</max_bytes><max_files>3</max_files></logs></supervisor>
XML
sysg start --config "$CONFIG" --daemonize
check "$?" "restart with the edited supervisor.xml exits 0"
sleep 4
sysg stop --project demo >/dev/null 2>&1
check "$?" "chatty project stops before log inspection"
grep -q '^  <logs>$' "$SUP_XML"
check "$?" "compact legacy supervisor.xml is normalized"
grep -q '<max_bytes>4096</max_bytes>' "$SUP_XML"
check "$?" "normalization preserves the custom log cap"
grep -q '<pre_start_secs>300</pre_start_secs>' "$SUP_XML"
check "$?" "legacy supervisor.xml receives timeout defaults"

echo "--- log dir contents ---"; ls -la "$LOG_DIR" 2>/dev/null
# The active file must be bounded near the tiny cap (well under, say, 64KB), and
# at least one rotated file must exist — both prove the 4KB default took effect.
ACTIVE="$(ls -1 "$LOG_DIR"/chatty*.log 2>/dev/null | head -1)"
ROTATED_COUNT="$(ls -1 "$LOG_DIR"/chatty* 2>/dev/null | grep -cE '\.log\.[0-9]+$|\.[0-9]+\.log$|\.[0-9]+$' || true)"
echo "active=$ACTIVE rotated_count=$ROTATED_COUNT"
[ "$ROTATED_COUNT" -ge 1 ]
check "$?" "service log ROTATED at the tiny supervisor default (rotated file present)"
if [ -n "$ACTIVE" ]; then
  SIZE="$(wc -c < "$ACTIVE" 2>/dev/null || echo 999999)"
  echo "active log size: ${SIZE} bytes (cap 4096)"
  [ "$SIZE" -lt 65536 ]
  check "$?" "active log stays bounded near the 4KB cap (not the 10MB default)"
else
  check 1 "active log file exists"
fi

sysg stop --supervisor >/dev/null 2>&1
finish
