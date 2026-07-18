#!/usr/bin/env bash
# test-local.sh — install a locally-built sysg into the versioned install scheme
# so you can dogfood it on real projects before publishing, then revert cleanly.
#
# The install layout (macOS):
#   ~/.sysg/bin/sysg          -> ~/.local/bin/sysg   (on PATH)
#   ~/.local/bin/sysg         -> ~/.sysg/versions/<VER>/sysg
#   ~/.sysg/active-version    holds the active <VER> string
#
# Why this script exists: a naive `cp target/release/sysg ~/.local/bin/sysg`
# writes THROUGH the symlink, clobbers the signed slot, and macOS SIGKILLs the
# binary (adhoc code-signature no longer matches the bytes). This installs into
# a real per-version dir and re-signs, matching the scheme.
#
# Usage:
#   scripts/test-local.sh [--version VER] [--no-build]
#                       Build (unless --no-build) and install target/release/sysg.
#                       VER defaults to the built binary's own `--version`; pass
#                       --version to override the label it installs under.
#   scripts/test-local.sh --revert            restore the pre-install version
#   scripts/test-local.sh --revert --clean    ...and delete the tested version dir
#   scripts/test-local.sh --activate VER      activate an existing verified slot
#
# By DEFAULT this rebuilds (cargo build --release) first, so it always installs
# the latest code — pass --no-build to skip and use the existing binary as-is.
set -euo pipefail

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "${SCRIPT_DIR}/.." && pwd)
SYSG_HOME="${HOME}/.sysg"
VERSIONS_DIR="${SYSG_HOME}/versions"
ACTIVE_FILE="${SYSG_HOME}/active-version"
LINK="${HOME}/.local/bin/sysg"
PREV_MARKER="${SYSG_HOME}/.test-local-prev"
BUILT_BIN="${REPO_ROOT}/target/release/sysg"
RUNTIME_DIR="${HOME}/.local/share/systemg"
SUPERVISOR_PID_FILE="${RUNTIME_DIR}/sysg.pid"
CONTROL_SOCKET="${RUNTIME_DIR}/control.sock"

err()  { printf '\033[0;31m%s\033[0m\n' "$*" >&2; }
info() { printf '\033[0;36m%s\033[0m\n' "$*"; }
ok()   { printf '\033[0;32m%s\033[0m\n' "$*"; }

# Refuse to swap the binary while a supervisor is resident — a version mismatch
# between the CLI and a live daemon triggers a recycle of the running stack.
guard_no_supervisor() {
  local pid=""
  if [ -r "${SUPERVISOR_PID_FILE}" ]; then
    pid="$(tr -d '[:space:]' < "${SUPERVISOR_PID_FILE}")"
  fi
  if { [ -n "${pid}" ] && kill -0 "${pid}" 2>/dev/null; } \
     || [ -S "${CONTROL_SOCKET}" ]; then
    err "A sysg supervisor is currently running${pid:+ (PID ${pid})}."
    err "Stop it first with 'sysg stop --supervisor', then retry."
    exit 1
  fi
}

binary_version() {
  "$1" --version 2>/dev/null | awk '{print $NF}'
}

repoint() {
  # repoint <version> — point the symlink + active-version at an installed version
  local version="$1"
  local target="${VERSIONS_DIR}/${version}/sysg"
  if [ ! -x "${target}" ]; then
    err "Version '${version}' is not installed at ${target}"
    exit 1
  fi
  local actual
  actual="$(binary_version "${target}")"
  if [ "${actual}" != "${version}" ]; then
    err "Version slot '${version}' contains binary '${actual:-unknown}'."
    err "Refusing to activate a mislabeled or corrupted slot."
    exit 1
  fi
  ln -sfn "${target}" "${LINK}"
  printf '%s\n' "${version}" > "${ACTIVE_FILE}"
}

do_revert() {
  local clean="${1:-}"
  guard_no_supervisor
  if [ ! -f "${PREV_MARKER}" ]; then
    err "No test-local install to revert (marker ${PREV_MARKER} not found)."
    exit 1
  fi
  local prev tested
  prev="$(sed -n '1p' "${PREV_MARKER}")"
  tested="$(sed -n '2p' "${PREV_MARKER}")"
  info "Reverting to '${prev}' (was testing '${tested}')..."
  repoint "${prev}"
  rm -f "${PREV_MARKER}"
  if [ "${clean}" = "--clean" ] && [ -n "${tested}" ] \
     && [ "${tested}" != "${prev}" ]; then
    rm -rf "${VERSIONS_DIR:?}/${tested}"
    info "Removed tested version dir ${VERSIONS_DIR}/${tested}"
  fi
  ok "Reverted. Active: $(cat "${ACTIVE_FILE}") -> $("${LINK}" --version)"
}

do_install() {
  local want_version="$1"
  local do_build="$2"
  guard_no_supervisor

  # Rebuild by default so we never install a stale binary — the whole point of
  # dogfooding is to run the LATEST code, not whatever was last compiled.
  if [ "${do_build}" = "yes" ]; then
    info "Building release binary (cargo build --release)..."
    ( cd "${REPO_ROOT}" && cargo build --release ) \
      || { err "cargo build --release failed"; exit 1; }
  fi

  guard_no_supervisor

  if [ ! -f "${BUILT_BIN}" ]; then
    err "No built binary at ${BUILT_BIN}. Run: cargo build --release (or drop --no-build)."
    exit 1
  fi

  # Determine the version to install under: --version override, else what the
  # freshly built binary reports.
  local binary_version version
  binary_version="$(binary_version "${BUILT_BIN}")"
  if [ -n "${want_version}" ]; then
    version="${want_version}"
    if [ "${binary_version}" != "${version}" ]; then
      err "Refusing to install binary '${binary_version:-unknown}' as version '${version}'."
      exit 1
    fi
  else
    version="${binary_version}"
  fi
  if [ -z "${version}" ]; then
    err "Could not determine version. Pass it explicitly: $0 --version <VER>"
    exit 1
  fi

  local current="unknown"
  [ -f "${ACTIVE_FILE}" ] && current="$(cat "${ACTIVE_FILE}")"

  info "Installing local ${BUILT_BIN} as version '${version}' (was '${current}')..."
  local dest_dir="${VERSIONS_DIR}/${version}"
  local staged="${dest_dir}/.sysg.$$"
  mkdir -p "${dest_dir}"
  install -m 755 "${BUILT_BIN}" "${staged}"

  # Re-sign adhoc so macOS Gatekeeper doesn't SIGKILL the freshly written Mach-O.
  if command -v codesign >/dev/null 2>&1; then
    if codesign --force --sign - "${staged}" >/dev/null 2>&1; then
      info "adhoc-signed ${dest_dir}/sysg"
    else
      rm -f "${staged}"
      err "codesign failed; the existing version slot was left untouched"
      exit 1
    fi
  fi

  if [ ! -f "${PREV_MARKER}" ]; then
    printf '%s\n%s\n' "${current}" "${version}" > "${PREV_MARKER}"
  fi

  mv -f "${staged}" "${dest_dir}/sysg"
  repoint "${version}"
  ok "Installed. Active: $(cat "${ACTIVE_FILE}") -> $("${LINK}" --version)"
  info "Revert any time: $0 --revert   (add --clean to also remove the dir)"
}

WANT_VERSION=""
DO_BUILD="yes"
REVERT=""
CLEAN=""
ACTIVATE=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --version)
      shift
      [ "$#" -gt 0 ] || { err "--version needs a value, e.g. --version 0.55.1"; exit 1; }
      WANT_VERSION="$1"
      ;;
    --version=*) WANT_VERSION="${1#*=}" ;;
    --no-build)  DO_BUILD="no" ;;
    --revert)    REVERT="yes" ;;
    --activate)
      shift
      [ "$#" -gt 0 ] || { err "--activate needs a version"; exit 1; }
      ACTIVATE="$1"
      ;;
    --activate=*) ACTIVATE="${1#*=}" ;;
    --clean)     CLEAN="--clean" ;;
    --help|-h)
      sed -n '2,24p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) err "unknown option: $1"; exit 1 ;;
  esac
  shift
done

if [ -n "${REVERT}" ] && [ -n "${ACTIVATE}" ]; then
  err "--revert and --activate cannot be combined"
  exit 1
elif [ -n "${REVERT}" ]; then
  do_revert "${CLEAN}"
elif [ -n "${ACTIVATE}" ]; then
  guard_no_supervisor
  repoint "${ACTIVATE}"
  ok "Activated. Active: $(cat "${ACTIVE_FILE}") -> $("${LINK}" --version)"
else
  do_install "${WANT_VERSION}" "${DO_BUILD}"
fi
