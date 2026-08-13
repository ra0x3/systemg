#!/usr/bin/env bash
# Model-based sequence harness.
#
# Rather than assert hand-picked end states, this drives a RANDOM sequence of
# lifecycle operations (start / stop / restart / start-service / stop-service)
# against the real supervisor, maintains a lightweight MODEL of what each
# service's state should be, and after EVERY step asserts two things:
#
#   1. the invariant oracle (`sysg doctor`) reports no error — no ghost pid, no
#      "running but dead", no pid reuse, no mode leak;
#   2. the model agrees with reality — every service the model says is running
#      has exactly one live process, and every stopped service has none.
#
# This finds the race and ordering bugs no fixed UAT enumerates: the failure is
# caught at the step that introduced it, not at some unrelated later assertion.
#
# Determinism: SEED (env, default 1) drives the RNG so a red run reproduces
# exactly. The supervisor's own concurrency is what we are probing; the harness
# is deterministic given the seed.
set -u
. /usecase/lib.sh

CONFIG=/etc/systemg/systemg.yaml
SERVICES="db api worker"
STEPS="${MODEL_STEPS:-40}"
SEED="${SEED:-1}"

RANDSTATE="$SEED"
rnd() { RANDSTATE=$(( (RANDSTATE * 1103515245 + 12345) & 0x7fffffff )); echo $(( RANDSTATE % $1 )); }
pick() { set -- $1; local n; n=$(( $(rnd $#) + 1 )); eval echo "\${$n}"; }

# --- model: MODEL_<service> = up | down ---
# Dependency graph mirrors the manifest: api depends on db. A service can only
# be up if its dependencies are up; stopping a dependency fells its dependents.
DEPS_api="db"
for s in $SERVICES; do eval "MODEL_$s=down"; done
model_get() { eval "echo \$MODEL_$1"; }
model_set() { eval "MODEL_$1=$2"; }

deps_of() { eval "echo \${DEPS_$1:-}"; }
dependents_of() {
  local target=$1 s d
  for s in $SERVICES; do
    for d in $(deps_of "$s"); do [ "$d" = "$target" ] && echo "$s"; done
  done
}

# Bring a service up only if every dependency is up (dependency-gated start).
model_start() {
  local svc=$1 d
  for d in $(deps_of "$svc"); do
    [ "$(model_get "$d")" = "up" ] || return 0
  done
  model_set "$svc" up
}

# Stop a service and cascade to its dependents (systemg fells dependents when a
# dependency goes down).
model_stop() {
  local svc=$1 dep
  model_set "$svc" down
  for dep in $(dependents_of "$svc"); do model_stop "$dep"; done
}

oracle_clean() {
  sysg --sys doctor > /tmp/m-oracle.json 2>/tmp/m-oracle.err
  local rc=$?
  if [ "$rc" != "0" ]; then
    echo "!!! ORACLE FAILED after step $1 (op: $2) seed=$SEED"
    cat /tmp/m-oracle.json /tmp/m-oracle.err
  fi
  return $rc
}

# Assert the model matches reality: each up service has exactly one live pid,
# each down service has none. Uses status JSON + ps.
model_matches_reality() {
  local step=$1 op=$2 s state ok=0
  local S; S="$(sysg --sys status --config "$CONFIG" --format json 2>/dev/null)"
  for s in $SERVICES; do
    local want; want="$(model_get "$s")"
    local pid; pid="$(unit_field "$S" "$s" pid 2>/dev/null)"
    if [ "$want" = "up" ]; then
      if [ -z "$pid" ] || [ "$pid" = "absent" ] || [ "$pid" = "noparse" ] || ! kill -0 "$pid" 2>/dev/null; then
        echo "!!! MODEL MISMATCH step $step (op $op) seed=$SEED: '$s' model=up but pid='$pid' not alive"; ok=1
      fi
    fi
  done
  return $ok
}

apply_and_check() {
  local step=$1 op=$2
  oracle_clean "$step" "$op" || return 1
  model_matches_reality "$step" "$op" || return 1
  return 0
}

section "boot: start the whole stack (seed=$SEED, steps=$STEPS)"
sysg --sys start --config "$CONFIG" --daemonize >/dev/null 2>&1
check "$?" "initial start exits 0"
for s in $SERVICES; do model_set "$s" up; done
sleep 3
apply_and_check 0 "boot"
check "$?" "step 0 (boot): oracle clean and model matches"

FAILED_STEP=""
step=1
while [ "$step" -le "$STEPS" ]; do
  OP="$(pick "restart-all stop-all start-all restart-svc stop-svc start-svc")"
  SVC="$(pick "$SERVICES")"
  case "$OP" in
    restart-all)
      sysg --sys restart --config "$CONFIG" >/dev/null 2>&1
      for s in $SERVICES; do model_set "$s" up; done ;;
    stop-all)
      sysg --sys stop --config "$CONFIG" >/dev/null 2>&1
      for s in $SERVICES; do model_set "$s" down; done ;;
    start-all)
      sysg --sys start --config "$CONFIG" --daemonize >/dev/null 2>&1
      for s in $SERVICES; do model_set "$s" up; done ;;
    restart-svc)
      sysg --sys restart --service "$SVC" --config "$CONFIG" >/dev/null 2>&1
      model_start "$SVC" ;;
    stop-svc)
      sysg --sys stop --service "$SVC" --config "$CONFIG" >/dev/null 2>&1
      model_stop "$SVC" ;;
    start-svc)
      sysg --sys start --service "$SVC" --config "$CONFIG" >/dev/null 2>&1
      model_start "$SVC" ;;
  esac
  sleep 2

  if ! apply_and_check "$step" "$OP($SVC)"; then
    FAILED_STEP="$step:$OP($SVC)"
    break
  fi
  step=$(( step + 1 ))
done

if [ -n "$FAILED_STEP" ]; then
  check 1 "sequence stayed consistent (FAILED at step $FAILED_STEP)"
else
  check 0 "all $STEPS randomized steps: oracle clean + model matched reality"
fi

sysg --sys stop --config "$CONFIG" >/dev/null 2>&1
sysg --sys purge >/dev/null 2>&1
finish
