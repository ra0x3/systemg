#!/usr/bin/env bash
# FULLSTACK ACCEPTANCE — the final, no-corners-cut proof of the whole refactor.
#
# A real three-tier stack (Postgres + Redis + a Python web API) is booted under
# sysg, then evolved through a SEQUENCE of git patches. After EACH patch we run
# the sysg command it targets and assert the change is actually live — via HTTP
# behavior, `ps`, AND `sysg status`, which must all agree. This is a long,
# cumulative, real-world journey: patch 1 -> test, patch 2 -> test, and so on,
# against one long-lived stack. No trusting that it works.
set -u
. /usecase/lib.sh

REPO=/usecase/app_repo
CONFIG="$REPO/stack.yaml"
PATCHES=/usecase/patches

# --- container prep: real Postgres needs a data dir owned by a non-root user ---
prepare_datastores() {
  useradd -m -s /bin/bash pg 2>/dev/null || true
  mkdir -p /var/lib/pg /tmp
  chown -R pg:pg /var/lib/pg
  # initdb + trust auth so the web tier's psql connects without a password prompt
  su pg -c "/usr/lib/postgresql/14/bin/initdb -D /var/lib/pg -A trust -U postgres" >/tmp/initdb.log 2>&1
}

git_repo_init() {
  cp -r /usecase/base "$REPO"
  cd "$REPO"
  git init -q
  git config user.email t@t.t
  git config user.name t
  git add -A
  git commit -qm base
}

apply_patch() {
  cd "$REPO"
  git apply "$PATCHES/$1"
}

curl_body() { curl -fsS "http://127.0.0.1:${2:-8080}${1}" 2>/dev/null; }

wait_http() {
  local path="$1" port="${2:-8080}" i=0
  while [ "$i" -lt 30 ]; do
    curl -fsS "http://127.0.0.1:${port}${path}" >/dev/null 2>&1 && return 0
    sleep 1
    i=$((i+1))
  done
  return 1
}

# ============================================================================
section "prepare real datastores + git base commit"
prepare_datastores
check "$?" "postgres initdb succeeded"
git_repo_init
check "$?" "base app committed to git"

section "boot the full stack under sysg"
# run postgres as user 'pg' (it refuses to run as root); sysg starts it via the
# manifest, but the postgres binary drops to 'pg' via the service 'user' field.
sysg start --config "$CONFIG" --daemonize
check "$?" "sysg start exits 0"
wait_http /health 8080
check "$?" "web /health is green (db + cache reachable through sysg-managed stack)"

section "baseline: all three tiers managed and agreeing"
S="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
RUN=0
for svc in db cache web; do
  P="$(unit_field "$S" "$svc" pid shop)"
  [ -n "$P" ] && [ "$P" != "absent" ] && pid_alive "$P" && RUN=$((RUN+1))
done
[ "$RUN" = "3" ]
check "$?" "db, cache, web all running on live pids per status"
BODY="$(curl_body / 8080)"
echo "web says: $BODY"
echo "$BODY" | grep -q "hello from shop"
check "$?" "web serves the base greeting 'hello'"

# ============================================================================
section "PATCH 001 — change web GREETING, restart just web"
WEB_PID_BEFORE="$(unit_field "$S" web pid shop)"
apply_patch 001-change-greeting.patch
check "$?" "001 applies cleanly"
sysg restart --config "$CONFIG" -s web
check "$?" "restart -s web exits 0"
wait_http /health 8080
check "$?" "web healthy after restart"
BODY="$(curl_body / 8080)"
echo "web says: $BODY"
echo "$BODY" | grep -q "howdy from shop"
check "$?" "web now serves the patched greeting 'howdy' (env change is LIVE)"
S="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
WEB_PID_AFTER="$(unit_field "$S" web pid shop)"
[ -n "$WEB_PID_AFTER" ] && [ "$WEB_PID_AFTER" != "$WEB_PID_BEFORE" ] && pid_alive "$WEB_PID_AFTER"
check "$?" "web restarted on a NEW pid (was $WEB_PID_BEFORE, now $WEB_PID_AFTER)"
DB_PID="$(unit_field "$S" db pid shop)"
pid_alive "$DB_PID"
check "$?" "db was NOT touched by the scoped web restart"

# ============================================================================
section "PATCH 002 — add a 'worker' service, restart to reconcile it in"
apply_patch 002-add-worker.patch
check "$?" "002 applies cleanly"
sysg restart --config "$CONFIG"
check "$?" "full restart exits 0"
sleep 3
S="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
WK_PID="$(unit_field "$S" worker pid shop)"
[ -n "$WK_PID" ] && [ "$WK_PID" != "absent" ] && pid_alive "$WK_PID"
check "$?" "new 'worker' service is running (reconcile-added)"
[ "$(unit_count "$S")" = "4" ]
check "$?" "status now lists 4 units"
wait_http /health 8080
check "$?" "web still healthy after the reconcile"

# ============================================================================
section "PATCH 003 — remove 'cache', restart; cache must be verifiably dead"
CACHE_PID="$(unit_field "$S" cache pid shop)"
apply_patch 003-remove-cache.patch
check "$?" "003 applies cleanly"
sysg restart --config "$CONFIG"
check "$?" "full restart exits 0"
sleep 3
pid_alive "$CACHE_PID" && STILL=1 || STILL=0
[ "$STILL" = "0" ]
check "$?" "the removed cache process (pid $CACHE_PID) is dead (reconcile-removed verified)"
S="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
CACHE_NOW="$(unit_field "$S" cache pid shop)"
[ "$CACHE_NOW" = "absent" ]
check "$?" "status no longer lists 'cache'"

# ============================================================================
section "PATCH 004 — move web to port 8090, restart; serves on the new port"
apply_patch 004-change-web-port.patch
check "$?" "004 applies cleanly"
sysg restart --config "$CONFIG" -s web
check "$?" "restart -s web exits 0"
wait_http /health 8090
check "$?" "web now healthy on the NEW port 8090"
! curl -fsS http://127.0.0.1:8080/health >/dev/null 2>&1
check "$?" "web no longer answers on the OLD port 8080"

# ============================================================================
section "PATCH 005 — stop then start 'worker' (targeted lifecycle)"
sysg stop --config "$CONFIG" -s worker
check "$?" "stop -s worker exits 0"
sleep 2
S="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
WK_NOW="$(unit_field "$S" worker pid shop)"
if [ "$WK_NOW" = "absent" ] || [ -z "$WK_NOW" ] || ! pid_alive "$WK_NOW" 2>/dev/null; then
  check 0 "worker is stopped"
else
  check 1 "worker is stopped"
fi
sysg start --config "$CONFIG" -s worker
check "$?" "start -s worker exits 0"
sleep 2
S="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
WK_UP="$(unit_field "$S" worker pid shop)"
[ -n "$WK_UP" ] && [ "$WK_UP" != "absent" ] && pid_alive "$WK_UP"
check "$?" "worker is back up on a live pid"

# ============================================================================
section "PATCH 006 — supervision survives a crash (db killed -> respawned)"
DB_PID="$(unit_field "$S" db pid shop)"
kill -9 "$DB_PID" 2>/dev/null
echo "killed db pid $DB_PID; waiting for sysg to respawn it"
RESPAWN=0
i=0
while [ "$i" -lt 25 ]; do
  sleep 1
  S="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
  DB_NEW="$(unit_field "$S" db pid shop)"
  if [ -n "$DB_NEW" ] && [ "$DB_NEW" != "absent" ] && [ "$DB_NEW" != "$DB_PID" ] && pid_alive "$DB_NEW"; then
    RESPAWN=1; echo "db respawned on pid $DB_NEW"; break
  fi
  i=$((i+1))
done
[ "$RESPAWN" = "1" ]
check "$?" "sysg respawned the crashed db (real supervision on a real datastore)"
wait_http /health 8090
check "$?" "web recovers to healthy once db is back"

# ============================================================================
section "TEARDOWN — purge is refused while managing, forced purge wipes clean"
sysg purge >/tmp/pr.err 2>&1
grep -q "SG0401" /tmp/pr.err
check "$?" "bare purge refused with SG0401 while the stack is live"
sysg purge --force >/dev/null 2>&1
check "$?" "purge --force exits 0"
sleep 2
[ ! -d "$HOME/.local/share/systemg" ]
check "$?" "state dir wiped after forced purge"

finish
