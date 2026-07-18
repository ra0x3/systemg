#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
CARGO_TOML="$REPO_ROOT/Cargo.toml"

usage() {
  cat <<'EOF'
Usage: scripts/version-bump.sh [--major | --minor | --patch]

Options:
  --major    Bump the package major version and reset minor/patch to 0
  --minor    Bump the package minor version and reset patch to 0
  --patch    Bump the package patch version
  --help     Show this help message
EOF
}

die() {
  printf '%s\n' "$1" >&2
  exit 1
}

BUMP_KIND=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --major)
      [ -z "$BUMP_KIND" ] || die "choose exactly one of --major, --minor, or --patch"
      BUMP_KIND="major"
      ;;
    --minor)
      [ -z "$BUMP_KIND" ] || die "choose exactly one of --major, --minor, or --patch"
      BUMP_KIND="minor"
      ;;
    --patch)
      [ -z "$BUMP_KIND" ] || die "choose exactly one of --major, --minor, or --patch"
      BUMP_KIND="patch"
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      usage >&2
      die "unknown option: $1"
      ;;
  esac
  shift
done

[ -n "$BUMP_KIND" ] || die "missing bump flag; use --major, --minor, or --patch"
[ -f "$CARGO_TOML" ] || die "missing Cargo.toml at $CARGO_TOML"

CURRENT_VERSION=$(
  awk '
    /^\[package\]/ { in_package = 1; next }
    /^\[/ && !/^\[package\]/ { in_package = 0 }
    in_package && /^[[:space:]]*version[[:space:]]*=/ {
      line = $0
      sub(/^[^"]*"/, "", line)
      sub(/".*$/, "", line)
      print line
      exit
    }
  ' "$CARGO_TOML"
)

[ -n "$CURRENT_VERSION" ] || die "could not find package.version in $CARGO_TOML"

IFS=. read -r MAJOR MINOR PATCH <<EOF
$CURRENT_VERSION
EOF

case "$MAJOR:$MINOR:$PATCH" in
  *[!0-9:]*|::|:*|*:)
    die "unsupported version format: $CURRENT_VERSION"
    ;;
esac

case "$BUMP_KIND" in
  major)
    MAJOR=$((MAJOR + 1))
    MINOR=0
    PATCH=0
    ;;
  minor)
    MINOR=$((MINOR + 1))
    PATCH=0
    ;;
  patch)
    PATCH=$((PATCH + 1))
    ;;
esac

NEW_VERSION="${MAJOR}.${MINOR}.${PATCH}"

if FILE_MODE=$(stat -f '%Lp' "$CARGO_TOML" 2>/dev/null); then
  :
else
  FILE_MODE=$(stat -c '%a' "$CARGO_TOML")
fi

TMP_FILE=$(mktemp "${TMPDIR:-/tmp}/version-bump.XXXXXX")
trap 'rm -f "$TMP_FILE"' EXIT INT TERM

awk -v new_version="$NEW_VERSION" '
  /^\[package\]/ { in_package = 1; print; next }
  /^\[/ && !/^\[package\]/ { in_package = 0 }
  in_package && /^[[:space:]]*version[[:space:]]*=/ && !updated {
    sub(/"[^"]*"/, "\"" new_version "\"")
    updated = 1
  }
  { print }
  END {
    if (!updated) {
      exit 1
    }
  }
' "$CARGO_TOML" > "$TMP_FILE" || die "failed to update package.version"

chmod "$FILE_MODE" "$TMP_FILE"
mv "$TMP_FILE" "$CARGO_TOML"
trap - EXIT INT TERM

printf 'Bumped systemg version: %s -> %s\n' "$CURRENT_VERSION" "$NEW_VERSION"

cd "$REPO_ROOT"
cargo build --release
