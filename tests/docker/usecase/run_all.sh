#!/usr/bin/env bash
# Builds the shared sysg base image once, then builds and runs every use-case in
# tests/docker/usecase/<case>/ (each a Dockerfile + run.sh). Reports per-case
# pass/fail and exits non-zero if any case is RED.
#
# Usage (from repo root):  tests/docker/usecase/run_all.sh [case ...]
# With no args, runs every case. With args, runs only the named cases.
set -u

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
USECASE_DIR="${REPO_ROOT}/tests/docker/usecase"

cd "${REPO_ROOT}"

echo "== building shared base image (sysg-usecase-base) =="
docker build -f tests/docker/usecase/Dockerfile.base -t sysg-usecase-base . || {
  echo "base image build failed"
  exit 1
}

if [ "$#" -gt 0 ]; then
  CASES=("$@")
else
  CASES=()
  for dir in "${USECASE_DIR}"/*/; do
    [ -f "${dir}/Dockerfile" ] || continue
    CASES+=("$(basename "${dir}")")
  done
fi

GREEN=()
RED=()
for case in "${CASES[@]}"; do
  dir="${USECASE_DIR}/${case}"
  if [ ! -f "${dir}/Dockerfile" ]; then
    echo "!! no Dockerfile for case '${case}', skipping"
    RED+=("${case}")
    continue
  fi
  echo
  echo "############################################################"
  echo "# CASE: ${case}"
  echo "############################################################"
  if ! docker build -f "${dir}/Dockerfile" -t "sysg-usecase-${case}" .; then
    echo "!! build failed for '${case}'"
    RED+=("${case}")
    continue
  fi
  if docker run --rm --init "sysg-usecase-${case}"; then
    GREEN+=("${case}")
  else
    RED+=("${case}")
  fi
done

echo
echo "############################################################"
echo "# SUMMARY"
echo "############################################################"
for case in "${GREEN[@]:-}"; do [ -n "${case}" ] && echo "  GREEN  ${case}"; done
for case in "${RED[@]:-}"; do [ -n "${case}" ] && echo "  RED    ${case}"; done

[ "${#RED[@]}" -eq 0 ]
