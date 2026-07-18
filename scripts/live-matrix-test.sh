#!/usr/bin/env bash
# Live mode-matrix test against the user's real projects.
#
# Exercises foreground vs daemonize vs mixed, plus status and logs under each
# condition, and checks that per-project operations hit ONLY their own project.
# Always tears everything down.
#
# Usage: scripts/live-matrix-test.sh
set -u

REPO="$(cd "$(dirname "$0")/.." && pwd)"
GC_CFG=~/dev/repos/gamecast/sysg.dev.yaml
ARB_CFG=~/dev/repos/arbitration/sysg.dev.yaml
PASS=0; FAIL=0

ok()  { PASS=$((PASS+1)); printf '  \033[0;32mPASS\033[0m %s\n' "$1"; }
bad() { FAIL=$((FAIL+1)); printf '  \033[0;31mFAIL\033[0m %s\n' "$1"; }
chk() { [ "$1" = "0" ] && ok "$2" || bad "$2"; }
sec() { printf '\n\033[1;36m== %s ==\033[0m\n' "$1"; }

gc_procs()  { pgrep -f '/target/release/gamecast ' | wc -l | tr -d ' '; }
arb_procs() { pgrep -f 'cargo watch -C arb-rs' | wc -l | tr -d ' '; }

nuke() {
  sysg stop --supervisor >/dev/null 2>&1
  sleep 2
  pkill -x sysg 2>/dev/null
  pkill -f 'sysg start' 2>/dev/null
  pkill -f 'target/release/gamecast' 2>/dev/null
  pkill -f 'cargo watch -C arb-rs' 2>/dev/null
  pkill -f 'arb-www-app' 2>/dev/null
  pkill -f 'wait-for-db.sh' 2>/dev/null
  sleep 2
}

# fg_start <config> <outfile> <pidfile> — foreground start under a PTY
fg_start() { python3 "$REPO/scripts/fgpty.py" "$1" "$2" "$3" 600 & }
# real_ctrl_c <pidfile> — deliver a genuine terminal Ctrl-C (SIGINT to the group)
real_ctrl_c() { : > "$1.ctl"; }

jq_unit() { # jq_unit <project> <unit> <field>
  sysg status --format json 2>/dev/null | python3 -c "
import json,sys
proj,unit,field=sys.argv[1],sys.argv[2],sys.argv[3]
d=json.load(sys.stdin)
for u in d.get('units',[]):
    if u['name']==unit and (u.get('project') or {}).get('id')==proj:
        if field=='pid': print((u.get('process') or {}).get('pid') or 'none')
        elif field=='mode': print((u.get('project') or {}).get('mode'))
        else: print(u.get(field))
        break
else: print('absent')" "$1" "$2" "$3" 2>/dev/null
}
proj_mode() { sysg status --format json 2>/dev/null | python3 -c "
import json,sys
d=json.load(sys.stdin)
for u in d.get('units',[]):
    p=u.get('project') or {}
    if p.get('id')==sys.argv[1]: print(p.get('mode')); break
else: print('absent')" "$1" 2>/dev/null; }

nuke

############################################################
sec "A. BOTH DAEMONIZE"
sysg start -c "$GC_CFG" --daemonize >/dev/null 2>&1
sysg start -c "$ARB_CFG" --daemonize >/dev/null 2>&1
sleep 14
[ "$(proj_mode gamecast-dev)" = "daemon" ]; chk $? "gamecast mode=daemon"
[ "$(proj_mode arbitration-dev)" = "daemon" ]; chk $? "arbitration mode=daemon"

# --- status under normal conditions ---
# status exit code REFLECTS health (non-zero when a unit is failed), so assert
# on content, not on rc. rc 2 with a genuinely-failed unit is correct behaviour.
sysg status >/dev/null 2>&1; [ $? -le 2 ]; chk $? "status exits 0-2 with two daemon projects"
S="$(sysg status 2>/dev/null)"
echo "$S" | grep -qi 'gamecast-dev'; chk $? "status lists gamecast-dev"
echo "$S" | grep -qi 'arbitration-dev'; chk $? "status lists arbitration-dev"
T0=$(date +%s); sysg status >/dev/null 2>&1; T1=$(date +%s)
[ "$((T1-T0))" -lt 5 ]; chk $? "status is fast (<5s) with 2 projects"
sysg status -p gamecast-dev 2>/dev/null | grep -qi 'gamecast'; chk $? "status -p gamecast-dev scopes to it"
sysg status -p gamecast-dev 2>/dev/null | grep -qi 'arb_rs'; [ $? -ne 0 ]; chk $? "status -p gamecast-dev excludes arbitration units"

# --- logs under normal conditions ---
sysg logs -p gamecast-dev -s gamecast_api --lines 20 --no-follow >/tmp/lg1.out 2>&1
chk $? "logs -p gamecast-dev -s gamecast_api exits 0"
[ -s /tmp/lg1.out ]; chk $? "logs returned output"
grep -qi 'arb_rs\|arb_www' /tmp/lg1.out; [ $? -ne 0 ]; chk $? "gamecast logs contain no arbitration lines"
sysg logs -p arbitration-dev --lines 20 --no-follow >/tmp/lg2.out 2>&1
chk $? "logs -p arbitration-dev (whole project) exits 0"
grep -qi 'gamecast_api\|gamecast_ingest' /tmp/lg2.out; [ $? -ne 0 ]; chk $? "arbitration logs contain no gamecast lines"
sysg logs --supervisor --lines 20 --no-follow >/tmp/lg3.out 2>&1
chk $? "logs --supervisor exits 0"
sysg logs >/tmp/lg4.out 2>&1; [ $? -ne 0 ]; chk $? "bare logs is refused (needs a target)"

# --- isolation ---
GC1="$(jq_unit gamecast-dev gamecast_api pid)"; ARB1="$(jq_unit arbitration-dev arb_rs__dev pid)"
sysg restart -p gamecast-dev -s gamecast_api >/dev/null 2>&1
sleep 6
GC2="$(jq_unit gamecast-dev gamecast_api pid)"; ARB2="$(jq_unit arbitration-dev arb_rs__dev pid)"
[ "$GC2" != "$GC1" ] && [ "$GC2" != "absent" ]; chk $? "restart -s changed gamecast_api pid ($GC1 -> $GC2)"
[ "$ARB2" = "$ARB1" ]; chk $? "arbitration untouched by gamecast restart"
sysg stop -p gamecast-dev >/dev/null 2>&1
sleep 4
[ "$(gc_procs)" = "0" ]; chk $? "stop -p gamecast killed all its procs"
pgrep -x sysg >/dev/null; chk $? "supervisor stays warm"
# status AFTER a project is stopped: must still render, and must not claim the
# stopped project's units are running.
sysg status >/tmp/st_after.out 2>&1; [ $? -le 2 ]; chk $? "status still renders after a project stopped"
grep -qi 'arbitration-dev' /tmp/st_after.out; chk $? "surviving project still listed after the other stopped"
# --format json must stay parseable even when nothing matches
sysg status -s no_such_unit_xyz --format json >/tmp/st_empty.out 2>&1
python3 -c "import json,sys;json.load(open('/tmp/st_empty.out'))" 2>/dev/null
chk $? "status --format json emits JSON (not prose) on an empty result"
nuke

############################################################
sec "B. BOTH FOREGROUND (real Ctrl-C)"
fg_start "$GC_CFG" /tmp/m_gc.out /tmp/m_gc.pid
sleep 16
fg_start "$ARB_CFG" /tmp/m_arb.out /tmp/m_arb.pid
sleep 16
[ "$(proj_mode gamecast-dev)" = "foreground" ]; chk $? "gamecast mode=foreground"
[ "$(proj_mode arbitration-dev)" = "foreground" ]; chk $? "arbitration mode=foreground (2nd fg project)"
sysg status >/dev/null 2>&1; chk $? "status exits 0 with two FOREGROUND projects"
! grep -qiE 'arb_rs__dev|arb_www' /tmp/m_gc.out 2>/dev/null; chk $? "gamecast fg terminal: no arbitration bleed"
! grep -qE 'DEBUG systemg|Supervisor received command' /tmp/m_gc.out 2>/dev/null; chk $? "gamecast fg terminal: no supervisor DEBUG"
sysg logs -p gamecast-dev -s gamecast_api --lines 10 --no-follow >/dev/null 2>&1; chk $? "logs works while projects are foreground"

real_ctrl_c /tmp/m_gc.pid
sleep 14
[ "$(gc_procs)" = "0" ]; chk $? "real Ctrl-C tore down the gamecast fg project"
[ "$(arb_procs)" -gt 0 ]; chk $? "arbitration fg project survived gamecast Ctrl-C"
pgrep -x sysg >/dev/null; chk $? "supervisor warm after one fg Ctrl-C"
sysg status >/dev/null 2>&1; chk $? "status exits 0 after a fg project was Ctrl-C'd"
nuke

############################################################
sec "C. MIXED: gamecast FG + arbitration DAEMON"
fg_start "$GC_CFG" /tmp/m_gc2.out /tmp/m_gc2.pid
sleep 16
sysg start -c "$ARB_CFG" --daemonize >/dev/null 2>&1
sleep 14
[ "$(proj_mode gamecast-dev)" = "foreground" ]; chk $? "gamecast=foreground (mixed)"
[ "$(proj_mode arbitration-dev)" = "daemon" ]; chk $? "arbitration=daemon (mixed)"
sysg status >/dev/null 2>&1; chk $? "status exits 0 in mixed mode"
! grep -qiE 'arb_rs__dev|arb_www' /tmp/m_gc2.out 2>/dev/null; chk $? "fg terminal: no daemon-project bleed"
GCM1="$(jq_unit gamecast-dev gamecast_api pid)"
sysg restart -p arbitration-dev >/dev/null 2>&1
sleep 12
[ "$(jq_unit gamecast-dev gamecast_api pid)" = "$GCM1" ]; chk $? "restarting the daemon project didn't touch the fg project"
sysg logs -p arbitration-dev --lines 10 --no-follow >/dev/null 2>&1; chk $? "logs on the daemon project works in mixed mode"
real_ctrl_c /tmp/m_gc2.pid
sleep 14
[ "$(gc_procs)" = "0" ]; chk $? "Ctrl-C tore down the fg project (mixed)"
[ "$(jq_unit arbitration-dev arb_rs__dev pid)" != "absent" ]; chk $? "daemon project survived the fg Ctrl-C"
nuke

############################################################
sec "D. MIXED: gamecast DAEMON + arbitration FG"
sysg start -c "$GC_CFG" --daemonize >/dev/null 2>&1
sleep 14
fg_start "$ARB_CFG" /tmp/m_arb2.out /tmp/m_arb2.pid
sleep 16
[ "$(proj_mode gamecast-dev)" = "daemon" ]; chk $? "gamecast=daemon (reverse mixed)"
[ "$(proj_mode arbitration-dev)" = "foreground" ]; chk $? "arbitration=foreground (reverse mixed)"
! grep -qiE 'gamecast_api|gamecast_ingest' /tmp/m_arb2.out 2>/dev/null; chk $? "arb fg terminal: no gamecast bleed"
sysg status >/dev/null 2>&1; chk $? "status exits 0 in reverse-mixed mode"
real_ctrl_c /tmp/m_arb2.pid
sleep 14
[ "$(jq_unit gamecast-dev gamecast_api pid)" != "absent" ]; chk $? "daemon project survived fg Ctrl-C (reverse)"
[ "$(arb_procs)" = "0" ]; chk $? "arbitration fg torn down by Ctrl-C"
nuke

############################################################
sec "E. STATUS/LOGS EDGE CONDITIONS"
# no supervisor at all
sysg status >/tmp/st_none.out 2>&1; RC=$?
[ "$RC" != "0" ]; chk $? "status exits non-zero with no supervisor"
grep -qi 'no running supervisor' /tmp/st_none.out; chk $? "status says 'no running supervisor'"
! grep -qi 'sysg logs' /tmp/st_none.out; chk $? "status failure does NOT suggest 'sysg logs'"
sysg logs -p gamecast-dev --lines 5 --no-follow >/tmp/lg_none.out 2>&1
chk $? "logs from disk still works with no supervisor"

printf '\n\033[1m== RESULT: %d passed, %d failed ==\033[0m\n' "$PASS" "$FAIL"
[ "$FAIL" = "0" ]
