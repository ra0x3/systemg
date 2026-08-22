#!/usr/bin/env bash
set -euo pipefail

# Renders the Homebrew formula for a published systemg release.
#
# Usage: scripts/brew/render-formula.sh <version> [output]
#
# <version> may carry a leading "v". [output] defaults to "-" (stdout).
#
# The four release tarballs the formula points at must already be published;
# this script downloads each one to hash it, so the checksums it writes are the
# checksums of the bytes Homebrew will actually fetch.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TEMPLATE="${SCRIPT_DIR}/sysg.rb.tmpl"

REPO="${SYSG_BREW_REPO:-ra0x3/systemg}"

if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
  echo "Usage: $0 <version> [output]" >&2
  exit 1
fi

VERSION="${1#v}"
OUTPUT="${2:--}"

if [ -z "$VERSION" ]; then
  echo "Refusing to render a formula for an empty version." >&2
  exit 1
fi

if [ ! -f "$TEMPLATE" ]; then
  echo "Formula template not found: $TEMPLATE" >&2
  exit 1
fi

DOWNLOAD_BASE="${SYSG_BREW_DOWNLOAD_BASE:-https://github.com/${REPO}/releases/download/v${VERSION}}"

SLOTS="
DARWIN_ARM64 aarch64-apple-darwin
DARWIN_X86_64 x86_64-apple-darwin
LINUX_ARM64 aarch64-unknown-linux-gnu
LINUX_X86_64 x86_64-unknown-linux-gnu
"

WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT INT TERM

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

RENDERED="${WORK_DIR}/sysg.rb"
cp "$TEMPLATE" "$RENDERED"

while read -r slot target; do
  [ -n "$slot" ] || continue

  asset="sysg-${VERSION}-${target}.tar.gz"
  url="${DOWNLOAD_BASE}/${asset}"
  archive="${WORK_DIR}/${asset}"

  echo "Hashing ${asset} ..." >&2
  if ! curl --proto '=https' --tlsv1.2 -fsSL --retry 3 --retry-delay 2 -o "$archive" "$url"; then
    echo "Could not download ${url}" >&2
    echo "Is v${VERSION} published with all of its release assets?" >&2
    exit 1
  fi

  digest="$(sha256_of "$archive")"
  if [ -z "$digest" ]; then
    echo "Could not hash ${archive}" >&2
    exit 1
  fi

  sed -i.bak "s|__SHA256_${slot}__|${digest}|g" "$RENDERED"
  rm -f "${RENDERED}.bak"
done <<< "$SLOTS"

sed -i.bak "s|__VERSION__|${VERSION}|g" "$RENDERED"
rm -f "${RENDERED}.bak"

if grep -q "__[A-Z0-9_]*__" "$RENDERED"; then
  echo "Rendered formula still holds unsubstituted placeholders:" >&2
  grep -o "__[A-Z0-9_]*__" "$RENDERED" | sort -u >&2
  exit 1
fi

if [ "$OUTPUT" = "-" ]; then
  cat "$RENDERED"
else
  mkdir -p "$(dirname "$OUTPUT")"
  cp "$RENDERED" "$OUTPUT"
  echo "Wrote ${OUTPUT}" >&2
fi
