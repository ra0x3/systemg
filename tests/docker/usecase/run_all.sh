#!/usr/bin/env bash
# Builds the shared sysg base image once, then builds and runs every use-case in
# tests/docker/usecase/<case>/ (each a Dockerfile + run.sh) IN PARALLEL. Reports
# per-case pass/fail and exits non-zero if any case is RED.
#
# Usage (from repo root):
#   tests/docker/usecase/run_all.sh [case ...]      # all cases, or just named ones
#   USECASE_JOBS=4 tests/docker/usecase/run_all.sh   # cap concurrency (default: nproc)
#
# Each case runs in its own container; per-case output is streamed to a temp log
# and printed under a banner once that case finishes, so parallel logs don't
# interleave.
set -u

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
USECASE_DIR="${REPO_ROOT}/tests/docker/usecase"
cd "${REPO_ROOT}"

# Max concurrent cases. Default to CPU count, floored at 1.
if command -v nproc >/dev/null 2>&1; then
  DEFAULT_JOBS="$(nproc)"
else
  DEFAULT_JOBS="4"
fi
JOBS="${USECASE_JOBS:-${DEFAULT_JOBS}}"
[ "${JOBS}" -ge 1 ] 2>/dev/null || JOBS=1

echo "== building shared base image (sysg-usecase-base) =="
if ! docker build -f tests/docker/usecase/Dockerfile.base -t sysg-usecase-base .; then
  echo "base image build failed"
  exit 1
fi

if [ "$#" -gt 0 ]; then
  CASES=("$@")
else
  CASES=()
  for dir in "${USECASE_DIR}"/*/; do
    [ -f "${dir}/Dockerfile" ] || continue
    CASES+=("$(basename "${dir}")")
  done
fi

if [ "${#CASES[@]}" -eq 0 ]; then
  echo "no cases found"
  exit 1
fi

LOG_DIR="$(mktemp -d)"
trap 'rm -rf "${LOG_DIR}"' EXIT

# Build + run one case, writing combined output to ${LOG_DIR}/<case>.log and its
# exit status to ${LOG_DIR}/<case>.rc (0 = GREEN).
run_case() {
  local case="$1"
  local dir="${USECASE_DIR}/${case}"
  local log="${LOG_DIR}/${case}.log"
  local rc=0
  {
    if [ ! -f "${dir}/Dockerfile" ]; then
      echo "!! no Dockerfile for case '${case}'"
      rc=2
    elif ! docker build -f "${dir}/Dockerfile" -t "sysg-usecase-${case}" "${REPO_ROOT}"; then
      echo "!! build failed for '${case}'"
      rc=2
    else
      docker run --rm --init "sysg-usecase-${case}"
      rc=$?
    fi
  } >"${log}" 2>&1
  echo "${rc}" >"${LOG_DIR}/${case}.rc"
}

echo "== running ${#CASES[@]} case(s), up to ${JOBS} in parallel =="

GREEN=()
RED=()
LAUNCHED=()
REPORTED=()

was_reported() {
  local target="$1"
  local case
  for case in "${REPORTED[@]:-}"; do
    [ "${case}" = "${target}" ] && return 0
  done
  return 1
}

report_case() {
  local case="$1"
  local rc
  echo
  echo "############################################################"
  echo "# CASE: ${case}"
  echo "############################################################"
  cat "${LOG_DIR}/${case}.log" 2>/dev/null || echo "(no output captured)"
  rc="$(cat "${LOG_DIR}/${case}.rc" 2>/dev/null || echo 1)"
  REPORTED+=("${case}")
  if [ "${rc}" = "0" ]; then
    GREEN+=("${case}")
  else
    RED+=("${case}")
  fi
}

report_done() {
  local case
  for case in "${LAUNCHED[@]:-}"; do
    [ -f "${LOG_DIR}/${case}.rc" ] || continue
    was_reported "${case}" || report_case "${case}"
  done
}

STOP=0
for case in "${CASES[@]}"; do
  report_done
  if [ "${#RED[@]}" -gt 0 ]; then
    STOP=1
  fi
  [ "${STOP}" = "0" ] || break
  run_case "${case}" &
  LAUNCHED+=("${case}")
  while [ "$(jobs -rp | wc -l)" -ge "${JOBS}" ]; do
    wait -n 2>/dev/null || true
    report_done
    if [ "${#RED[@]}" -gt 0 ]; then
      STOP=1
      break
    fi
  done
done

while [ "$(jobs -rp | wc -l)" -gt 0 ]; do
  wait -n 2>/dev/null || true
  report_done
done
report_done

echo
echo "############################################################"
echo "# SUMMARY"
echo "############################################################"
for case in "${GREEN[@]:-}"; do [ -n "${case}" ] && echo "  GREEN  ${case}"; done
for case in "${RED[@]:-}"; do [ -n "${case}" ] && echo "  RED    ${case}"; done

[ "${#RED[@]}" -eq 0 ]
