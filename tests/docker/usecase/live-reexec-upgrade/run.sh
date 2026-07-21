#!/usr/bin/env bash
set -u
. /usecase/lib.sh

export PATH="$HOME/.local/bin:$PATH"
BASE_URL="file:///release"
STATE_DIR="$HOME/.local/share/systemg"

section "install the live-reexec baseline"
SYSG_DOWNLOAD_BASE_URL="$BASE_URL" sh /usecase/index.sh --version 0.57.1 >/tmp/install-0.out 2>&1
check "$?" "0.57.1 installs through the public installer"
[ "$(sysg --version 2>/dev/null)" = "systemg 0.57.1" ]
check "$?" "PATH resolves to 0.57.1"

section "boot two foreground projects with real lifecycle gates"
python3 /usecase/fgcap.py /usecase/pipeline.yaml 300 /tmp/pipeline.out &
PIPELINE_FG=$!
i=0
while [ "$i" -lt 30 ]; do
  SNAP="$(sysg status --format json 2>/dev/null || true)"
  [ "$(unit_field "$SNAP" worker state pipeline)" = "running" ] && break
  sleep 1
  i=$((i + 1))
done
[ "$(unit_field "$SNAP" probe state pipeline)" = "done" ] \
  && [ "$(unit_field "$SNAP" migration state pipeline)" = "done" ] \
  && [ "$(unit_field "$SNAP" api state pipeline)" = "running" ] \
  && [ "$(unit_field "$SNAP" worker state pipeline)" = "running" ]
check "$?" "pipeline completed one-shots, pre-start, health, and dependency gates"

python3 /usecase/fgcap.py /usecase/surface.yaml 300 /tmp/surface.out &
SURFACE_FG=$!
i=0
while [ "$i" -lt 20 ]; do
  SNAP="$(sysg status --format json 2>/dev/null || true)"
  [ "$(unit_field "$SNAP" ui state surface)" = "running" ] \
    && [ "$(unit_field "$SNAP" rollup state surface)" != "absent" ] \
    && break
  sleep 1
  i=$((i + 1))
done
[ "$(unit_field "$SNAP" ui state surface)" = "running" ]
check "$?" "second foreground project attached to the resident supervisor"

SUPERVISOR_BEFORE="$(tr -d ' ' < "$STATE_DIR/sysg.pid")"
SNAP_BEFORE="$(sysg status --format json 2>/dev/null)"
API_BEFORE="$(unit_field "$SNAP_BEFORE" api pid pipeline)"
EMITTER_BEFORE="$(unit_field "$SNAP_BEFORE" emitter pid pipeline)"
PARTIAL_BEFORE="$(unit_field "$SNAP_BEFORE" partial pid pipeline)"
WORKER_BEFORE="$(unit_field "$SNAP_BEFORE" worker pid pipeline)"
UI_BEFORE="$(unit_field "$SNAP_BEFORE" ui pid surface)"
API_START="$(awk '{print $22}' "/proc/$API_BEFORE/stat")"
EMITTER_START="$(awk '{print $22}' "/proc/$EMITTER_BEFORE/stat")"
PARTIAL_START="$(awk '{print $22}' "/proc/$PARTIAL_BEFORE/stat")"
WORKER_START="$(awk '{print $22}' "/proc/$WORKER_BEFORE/stat")"
UI_START="$(awk '{print $22}' "/proc/$UI_BEFORE/stat")"
MODES_BEFORE="$(printf '%s' "$SNAP_BEFORE" | python3 -c 'import json,sys; d=json.load(sys.stdin); print(",".join(sorted({"{}:{}".format(u["project"]["id"], u["project"]["mode"]) for u in d["units"]})))')"
[ "$MODES_BEFORE" = "pipeline:foreground,surface:foreground" ]
check "$?" "both project modes are foreground before upgrade"

section "an active cron run refuses activation without changing PATH"
i=0
while [ "$i" -lt 25 ] && [ ! -f /tmp/cron-active ]; do
  sleep 1
  i=$((i + 1))
done
[ -f /tmp/cron-active ]
check "$?" "cron entered an active run"
SYSG_DOWNLOAD_BASE_URL="$BASE_URL" sh /usecase/index.sh --version 0.58.0 >/tmp/install-busy.out 2>&1
BUSY_RC=$?
[ "$BUSY_RC" -ne 0 ]
check "$?" "installer refuses a live handoff during cron execution"
grep -q SG0503 /tmp/install-busy.out
check "$?" "busy handoff reports SG0503"
[ "$(cat "$HOME/.sysg/active-version")" = "0.57.1" ] \
  && [ "$(tr -d ' ' < "$STATE_DIR/sysg.pid")" = "$SUPERVISOR_BEFORE" ]
check "$?" "failed activation leaves version and supervisor PID unchanged"

while [ -f /tmp/cron-active ]; do sleep 1; done
CRON_BEFORE="$(wc -l < /tmp/cron-runs | tr -d ' ')"
SNAP_BUSY="$(sysg status --format json 2>/dev/null)"
[ "$(unit_field "$SNAP_BUSY" api pid pipeline)" = "$API_BEFORE" ] \
  && [ "$(unit_field "$SNAP_BUSY" emitter pid pipeline)" = "$EMITTER_BEFORE" ] \
  && [ "$(unit_field "$SNAP_BUSY" partial pid pipeline)" = "$PARTIAL_BEFORE" ] \
  && [ "$(unit_field "$SNAP_BUSY" worker pid pipeline)" = "$WORKER_BEFORE" ] \
  && [ "$(unit_field "$SNAP_BUSY" ui pid surface)" = "$UI_BEFORE" ]
check "$?" "refused activation did not restart any workload"

section "retry through the installer with an unterminated log line in flight"
touch /tmp/release-partial
i=0
while [ "$i" -lt 10 ] && [ ! -f /tmp/partial-ready ]; do
  sleep 1
  i=$((i + 1))
done
[ -f /tmp/partial-ready ]
check "$?" "partial service has written an unterminated line"
SYSG_DOWNLOAD_BASE_URL="$BASE_URL" sh /usecase/index.sh --version 0.58.0 >/tmp/install-1.out 2>&1
UPGRADE_RC=$?
cat /tmp/install-1.out
[ "$UPGRADE_RC" -eq 0 ] && grep -q "Supervisor upgraded in place to 0.58.0" /tmp/install-1.out
check "$?" "public installer completed cross-minor same-PID live re-execution"
touch /tmp/finish-partial
sleep 5

SUPERVISOR_AFTER="$(tr -d ' ' < "$STATE_DIR/sysg.pid")"
SNAP_AFTER="$(sysg status --format json 2>/dev/null)"
[ "$SUPERVISOR_AFTER" = "$SUPERVISOR_BEFORE" ] \
  && [ "$(cat "$HOME/.sysg/active-version")" = "0.58.0" ] \
  && [ "$(sysg --version 2>/dev/null)" = "systemg 0.58.0" ]
check "$?" "supervisor PID, active slot, and PATH agree on 0.58.0"

[ "$(unit_field "$SNAP_AFTER" api pid pipeline)" = "$API_BEFORE" ] \
  && [ "$(unit_field "$SNAP_AFTER" emitter pid pipeline)" = "$EMITTER_BEFORE" ] \
  && [ "$(unit_field "$SNAP_AFTER" partial pid pipeline)" = "$PARTIAL_BEFORE" ] \
  && [ "$(unit_field "$SNAP_AFTER" worker pid pipeline)" = "$WORKER_BEFORE" ] \
  && [ "$(unit_field "$SNAP_AFTER" ui pid surface)" = "$UI_BEFORE" ]
check "$?" "every long-running workload kept its PID"

[ "$(awk '{print $22}' "/proc/$API_BEFORE/stat")" = "$API_START" ] \
  && [ "$(awk '{print $22}' "/proc/$EMITTER_BEFORE/stat")" = "$EMITTER_START" ] \
  && [ "$(awk '{print $22}' "/proc/$PARTIAL_BEFORE/stat")" = "$PARTIAL_START" ] \
  && [ "$(awk '{print $22}' "/proc/$WORKER_BEFORE/stat")" = "$WORKER_START" ] \
  && [ "$(awk '{print $22}' "/proc/$UI_BEFORE/stat")" = "$UI_START" ]
check "$?" "every workload kept its kernel start identity"

MODES_AFTER="$(printf '%s' "$SNAP_AFTER" | python3 -c 'import json,sys; d=json.load(sys.stdin); print(",".join(sorted({"{}:{}".format(u["project"]["id"], u["project"]["mode"]) for u in d["units"]})))')"
[ "$MODES_AFTER" = "$MODES_BEFORE" ] \
  && [ "$(unit_field "$SNAP_AFTER" probe state pipeline)" = "done" ] \
  && [ "$(unit_field "$SNAP_AFTER" migration state pipeline)" = "done" ]
check "$?" "foreground modes and completed one-shot states survived"

section "foreground streams reconnect without replaying static history"
pid_alive "$PIPELINE_FG" && pid_alive "$SURFACE_FG"
check "$?" "both foreground terminal drivers remain attached"
grep -qi reconnect /tmp/pipeline.out && grep -qi reconnect /tmp/surface.out
check "$?" "both foreground streams announced reconnection"
PROBE_ECHOES="$(grep -c PROBE_DONE /tmp/pipeline.out 2>/dev/null || true)"
MIGRATION_ECHOES="$(grep -c MIGRATION_DONE /tmp/pipeline.out 2>/dev/null || true)"
[ "${PROBE_ECHOES:-0}" -le 1 ] && [ "${MIGRATION_ECHOES:-0}" -le 1 ]
check "$?" "reconnect did not replay completed one-shot history"

section "log pipes retain sequence and partial-line boundaries"
EMITTER_LOG="$(sysg logs -p pipeline -s emitter --path 2>/dev/null | tail -n 1)"
python3 - "$EMITTER_LOG" <<'PY'
import re
import sys

values = []
with open(sys.argv[1], encoding="utf-8") as handle:
    for line in handle:
        match = re.search(r"SEQ:(\d+)\s*$", line)
        if match:
            values.append(int(match.group(1)))
if len(values) < 8 or values != list(range(values[0], values[-1] + 1)):
    print(f"captured sequence ({len(values)}): {values}")
    raise SystemExit(1)
PY
check "$?" "continuous service logs have no gap or duplicate across exec"
sysg logs -p pipeline -s partial --no-follow --raw -l 20 > /tmp/partial.log 2>/dev/null
grep -qx PARTIAL_COMPLETE /tmp/partial.log
check "$?" "unterminated bytes resumed as one complete log line"

section "cron scheduling resumes once without overlap"
i=0
CRON_AFTER="$CRON_BEFORE"
while [ "$i" -lt 25 ]; do
  CRON_AFTER="$(wc -l < /tmp/cron-runs | tr -d ' ')"
  [ "$CRON_AFTER" -gt "$CRON_BEFORE" ] && break
  sleep 1
  i=$((i + 1))
done
[ "$CRON_AFTER" -eq $((CRON_BEFORE + 1)) ]
check "$?" "the next cron boundary fired exactly once after upgrade"

section "the replacement supervisor owns restart and shutdown"
kill -9 "$WORKER_BEFORE"
i=0
WORKER_AFTER="$WORKER_BEFORE"
while [ "$i" -lt 20 ]; do
  sleep 1
  SNAP_RESTART="$(sysg status --format json 2>/dev/null || true)"
  WORKER_AFTER="$(unit_field "$SNAP_RESTART" worker pid pipeline)"
  [ "$WORKER_AFTER" != "$WORKER_BEFORE" ] \
    && [ "$WORKER_AFTER" != "absent" ] \
    && pid_alive "$WORKER_AFTER" \
    && break
  i=$((i + 1))
done
[ "$WORKER_AFTER" != "$WORKER_BEFORE" ] && pid_alive "$WORKER_AFTER"
check "$?" "replacement supervisor respawned a killed adopted service"

sysg stop --supervisor >/dev/null 2>&1
sleep 2
SURVIVORS=0
for pid in "$SUPERVISOR_BEFORE" "$API_BEFORE" "$EMITTER_BEFORE" "$PARTIAL_BEFORE" "$UI_BEFORE" "$WORKER_AFTER"; do
  pid_alive "$pid" && SURVIVORS=$((SURVIVORS + 1))
done
[ "$SURVIVORS" -eq 0 ]
check "$?" "final shutdown reaped the supervisor and every adopted workload"

finish
