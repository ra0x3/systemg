#!/usr/bin/env bash
# USE CASE: a cron run that fails is recorded as a failure, alerts once, and is
# not restarted off its schedule.
#
# WHAT THIS TESTS (real production bug, sysg 0.66.1)
#   A cron unit exiting non-zero reported `Done · Healthy · exit 0` on Linux.
#   The daemon monitor also waits on managed children, and a pidfd wakes it the
#   instant one exits, so it reaped the cron child first; the cron completion
#   thread then got ECHILD from its own waitpid and ASSUMED SUCCESS. The same
#   overlap had two more faces: the completion path never ran the unit's
#   `onerr` hook (so alerting was dead wherever the status was honest), and the
#   monitor treated the finished run as a crashed service and restarted it with
#   backoff — a once-a-minute job ran seven times in 135 seconds.
#
# WHY THIS SHIPPED
#   Every cron fixture in this suite exited 0, and no fixture anywhere declared
#   `hooks:`. The failure axis of a cron unit was untested end to end, so all
#   three faces lived in the seam between the two threads that judge a run.
#
# EXPECTED OUTCOME
#   - a failing cron run records `failed` with the real exit code,
#   - it never reports "completed successfully",
#   - `onerr` fires exactly once per run — not zero times, not twice,
#   - runs come from the schedule alone: no restart, no backoff re-run.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
rm -f /tmp/runs.log /tmp/alerts.log

sysg start -c "$CONFIG" --daemonize
check "$?" "cron project starts"

# Three boundaries of a 20s schedule, plus room for the 2s command.
sleep 65

section "the run is recorded as the failure it was"
S="$(sysg status --format json)"
[ "$(unit_field "$S" nightly state)" = "failed" ]
check "$?" "status reports the cron unit as failed"

EXIT_CODE="$(printf '%s' "$S" | python3 -c '
import json,sys
data=json.load(sys.stdin)
for u in data.get("units",[]):
    if u.get("name")=="nightly":
        print((u.get("last_exit") or {}).get("exit_code","absent")); break
else: print("absent")
')"
[ "$EXIT_CODE" = "4" ]
check "$?" "the recorded exit code is the command's own (got: $EXIT_CODE)"

sysg logs --supervisor 2>/dev/null | grep -q "Cron job 'nightly' failed: Process exited with code 4"
check "$?" "the supervisor log names the failure and its code"

! sysg logs --supervisor 2>/dev/null | grep -q "Cron job 'nightly' completed successfully"
check "$?" "a failed run is never reported as completed successfully"

section "the failure alerts exactly once per run"
RUNS="$(wc -l < /tmp/runs.log 2>/dev/null || echo 0)"
ALERTS="$(wc -l < /tmp/alerts.log 2>/dev/null || echo 0)"
[ "$RUNS" -ge 2 ]
check "$?" "the schedule fired at least twice in the window (runs: $RUNS)"
[ "$ALERTS" = "$RUNS" ]
check "$?" "onerr fired once per failed run (runs: $RUNS, alerts: $ALERTS)"

section "a cron unit is never restarted off its schedule"
! sysg logs --supervisor 2>/dev/null | grep -qE "Restarting 'nightly'|Service 'nightly' crashed"
check "$?" "no crash-restart for a unit whose next run is its schedule"

# A backoff re-run would land ~5s after the previous exit; the schedule is 20s.
CLOSEST="$(python3 -c '
import sys
try: t=[int(x) for x in open("/tmp/runs.log")]
except Exception: print(-1); sys.exit()
print(min((b-a for a,b in zip(t,t[1:])), default=999))
')"
[ "$CLOSEST" -ge 15 ]
check "$?" "consecutive runs are a schedule apart, not a backoff apart (${CLOSEST}s)"

sysg stop --supervisor >/dev/null 2>&1
finish
