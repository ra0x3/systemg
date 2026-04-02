#!/bin/bash

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BLUE='\033[0;34m'
BOLD='\033[1m'
RESET='\033[0m'

PIDS=()
PID_LABELS=()
PID_EXIT_CODES=()

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
TMP_DIR="$(mktemp -d)"

trap 'rm -rf "${TMP_DIR}"' EXIT INT TERM

run_labeled() {
  local label="$1"
  local color="$2"
  local cmd="$3"
  local exit_code_file="${TMP_DIR}/${label}.exit"

  (
    set -o pipefail
    eval "${cmd}" 2>&1 | while IFS= read -r line; do
      printf "${BOLD}${color}[%s]${RESET} %s\n" "${label}" "${line}"
    done
    echo "${PIPESTATUS[0]}" > "${exit_code_file}"
  ) &

  PIDS+=($!)
  PID_LABELS+=("${label}")
}

check_pid_result() {
  local pid="$1"
  local label="$2"
  local exit_code_file="${TMP_DIR}/${label}.exit"
  local exit_code

  wait "${pid}" 2>/dev/null
  exit_code=$?

  if [ -f "${exit_code_file}" ]; then
    exit_code="$(cat "${exit_code_file}" 2>/dev/null || echo "${exit_code}")"
  fi

  if [ "${exit_code}" -eq 0 ]; then
    printf "${GREEN}✓${RESET} ${GREEN}%s${RESET}\n" "${label}"
    PID_EXIT_CODES+=(0)
    return 0
  fi

  printf "${RED}✗${RESET} ${RED}%s${RESET}\n" "${label}"
  PID_EXIT_CODES+=(1)
  return 1
}

START_TIME="$(date +%s)"

printf "${BOLD}${BLUE}Starting concurrent commit checks...${RESET}\n\n"
printf "${BOLD}Launching checks...${RESET}\n"

run_labeled \
  "rust:fmt" \
  "${YELLOW}" \
  "cd \"${REPO_ROOT}\" && cargo +nightly fmt -- --check"

run_labeled \
  "rust:clippy" \
  "${GREEN}" \
  "cd \"${REPO_ROOT}\" && cargo clippy --all-targets --all-features -- -D warnings"

run_labeled \
  "docs:mintlify" \
  "${BLUE}" \
  "cd \"${REPO_ROOT}/docs\" && mintlify broken-links"

printf "\n${BOLD}Waiting for all checks to complete...${RESET}\n\n"

FAILED_CHECKS=()
PASSED_CHECKS=()
ANY_FAILED=0

for i in "${!PIDS[@]}"; do
  pid="${PIDS[$i]}"
  label="${PID_LABELS[$i]}"

  if check_pid_result "${pid}" "${label}"; then
    PASSED_CHECKS+=("${label}")
  else
    FAILED_CHECKS+=("${label}")
    ANY_FAILED=1
  fi
done

END_TIME="$(date +%s)"
ELAPSED="$((END_TIME - START_TIME))"

printf "\n${BOLD}${BLUE}========================================${RESET}\n"
printf "${BOLD}${BLUE}           SUMMARY${RESET}\n"
printf "${BOLD}${BLUE}========================================${RESET}\n\n"

if [ "${#PASSED_CHECKS[@]}" -gt 0 ]; then
  printf "${BOLD}${GREEN}✓ Passed checks (%s):${RESET}\n" "${#PASSED_CHECKS[@]}"
  for check in "${PASSED_CHECKS[@]}"; do
    printf "  ${GREEN}✓${RESET} %s\n" "${check}"
  done
fi

if [ "${#FAILED_CHECKS[@]}" -gt 0 ]; then
  printf "\n${BOLD}${RED}✗ Failed checks (%s):${RESET}\n" "${#FAILED_CHECKS[@]}"
  for check in "${FAILED_CHECKS[@]}"; do
    printf "  ${RED}✗${RESET} %s\n" "${check}"
  done
fi

printf "\n${BOLD}Time elapsed: %ss${RESET}\n" "${ELAPSED}"

if [ "${ANY_FAILED}" -eq 1 ]; then
  printf "\n${BOLD}${RED}Commit checks failed!${RESET}\n"
  exit 1
fi

printf "\n${BOLD}${GREEN}All commit checks passed!${RESET}\n"
exit 0
