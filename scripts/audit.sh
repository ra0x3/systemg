#!/bin/bash

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BOLD='\033[1m'
RESET='\033[0m'

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

if ! command -v cargo-audit >/dev/null 2>&1; then
  printf "${YELLOW}cargo-audit not found, installing...${RESET}\n"
  cargo install cargo-audit
fi

printf "${BOLD}${YELLOW}Auditing dependencies for known CVEs...${RESET}\n\n"

cd "${REPO_ROOT}"

if cargo audit "$@"; then
  printf "\n${BOLD}${GREEN}No known vulnerabilities found!${RESET}\n"
  exit 0
fi

printf "\n${BOLD}${RED}Vulnerable dependencies detected!${RESET}\n"
exit 1
