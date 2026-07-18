#!/usr/bin/env bash
# USE CASE (UNHAPPY): a dirtied manifest refuses BARE commands with SG0018.
#
# WHAT THIS TESTS
#   The manifest cache is a convenience, not a source of truth. Once you edit the
#   manifest on disk, running ANY command without -c would act on the
#   supervisor's stale cached copy — so sysg refuses with SG0018 until you
#   re-submit with -c. This guards every command (start/stop/restart/status/
#   inspect/logs), and asserts the refusal changes NOTHING.
#
# HARD INVARIANTS
#   - boot with -c, then edit the manifest on disk,
#   - bare restart / status / inspect / logs / stop / start ALL exit non-zero
#     naming SG0018,
#   - the running service is untouched by the refused commands.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

section "boot with -c (submits the manifest)"
sysg start --config "$CONFIG" --daemonize
check "$?" "start exits 0"
sleep 2
# track the service via ps so the checks never depend on a (soon-dirty) bare
# status call.
PID_BEFORE="$(pgrep -f 'GREETING_IS' | head -1)"
echo "web pid before: $PID_BEFORE"
[ -n "$PID_BEFORE" ]
check "$?" "web is running before the manifest is dirtied"

section "dirty the manifest on disk (change GREETING)"
python3 -c "p='$CONFIG'; s=open(p).read().replace('hello','howdy'); open(p,'w').write(s)"
check "$?" "manifest edited on disk"

section "every BARE command is refused with SG0018"
for cmd in "restart" "status" "inspect -s web" "logs -s web --no-follow" "stop -s web" "start -s web"; do
  # shellcheck disable=SC2086
  sysg $cmd >/tmp/c.out 2>/tmp/c.err
  RC=$?
  if [ "$RC" != "0" ] && grep -q "SG0018" /tmp/c.err; then
    check 0 "bare '$cmd' refused with SG0018"
  else
    echo "--- '$cmd' rc=$RC ---"; cat /tmp/c.err | head -3
    check 1 "bare '$cmd' refused with SG0018"
  fi
done

section "the refused commands changed nothing"
PID_AFTER="$(pgrep -f 'GREETING_IS' | head -1)"
echo "web pid after refusals: $PID_AFTER"
[ -n "$PID_AFTER" ] && [ "$PID_AFTER" = "$PID_BEFORE" ] && pid_alive "$PID_AFTER"
check "$?" "web is still running on its ORIGINAL pid (nothing bounced)"

sysg stop --supervisor >/dev/null 2>&1
finish
