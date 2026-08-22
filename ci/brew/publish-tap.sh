#!/usr/bin/env bash
set -euo pipefail

# Publishes the rendered Homebrew formula to the systemg tap.
#
# Usage: ci/brew/publish-tap.sh <version>
#
# Env:
#   HOMEBREW_TAP_TOKEN  required; a token carrying contents:write on the tap
#   HOMEBREW_TAP_REPO   defaults to ra0x3/homebrew-tap
#   HOMEBREW_TAP_REMOTE overrides the clone URL, which is how this is tested
#                       against a local bare repository
#
# Idempotent. A tap already holding exactly this formula is left untouched, so
# re-running a release never pushes an empty commit, and a release that failed
# after this job can be replayed safely.

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
RENDER="${REPO_ROOT}/scripts/brew/render-formula.sh"

TAP_REPO="${HOMEBREW_TAP_REPO:-ra0x3/homebrew-tap}"

if [ "$#" -ne 1 ]; then
  echo "Usage: $0 <version>" >&2
  exit 1
fi

VERSION="${1#v}"

if [ -z "$VERSION" ]; then
  echo "Refusing to publish a formula for an empty version." >&2
  exit 1
fi

TAP_REMOTE="${HOMEBREW_TAP_REMOTE:-}"

if [ -z "$TAP_REMOTE" ]; then
  if [ -z "${HOMEBREW_TAP_TOKEN:-}" ]; then
    echo "HOMEBREW_TAP_TOKEN is empty; cannot push to ${TAP_REPO}." >&2
    exit 1
  fi
  TAP_REMOTE="https://x-access-token:${HOMEBREW_TAP_TOKEN}@github.com/${TAP_REPO}.git"
fi

WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT INT TERM

RENDERED="${WORK_DIR}/sysg.rb"
"$RENDER" "$VERSION" "$RENDERED"

CLONE_DIR="${WORK_DIR}/tap"
git clone --depth 1 --quiet "$TAP_REMOTE" "$CLONE_DIR"
git -C "$CLONE_DIR" remote set-url origin "$(printf '%s' "$TAP_REMOTE" | sed 's|//[^@/]*@|//|')"

BRANCH="$(git -C "$CLONE_DIR" symbolic-ref --short HEAD)"
FORMULA="${CLONE_DIR}/Formula/sysg.rb"

if [ -f "$FORMULA" ] && cmp -s "$RENDERED" "$FORMULA"; then
  echo "${TAP_REPO} already serves sysg ${VERSION}; nothing to push."
  exit 0
fi

mkdir -p "$(dirname "$FORMULA")"
cp "$RENDERED" "$FORMULA"

git -C "$CLONE_DIR" config user.name "github-actions[bot]"
git -C "$CLONE_DIR" config user.email "41898282+github-actions[bot]@users.noreply.github.com"
git -C "$CLONE_DIR" add Formula/sysg.rb
git -C "$CLONE_DIR" commit --quiet -m "sysg ${VERSION}"
git -C "$CLONE_DIR" push --quiet "$TAP_REMOTE" "HEAD:refs/heads/${BRANCH}"

echo "Published sysg ${VERSION} to ${TAP_REPO} (${BRANCH})."
