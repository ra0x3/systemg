#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

echo "[cleanup] Resetting gen-ui example workspace..."

DIRS_TO_REMOVE=(
  "snapshots"
  "gen-ui"
  "dist"
  "src"
  "build"
  "node_modules"
  "coverage"
  ".next"
  ".cache"
  ".turbo"
  ".parcel-cache"
)

for dir in "${DIRS_TO_REMOVE[@]}"; do
  if [ -d "$dir" ]; then
    echo "[cleanup] Removing directory: $dir"
    rm -rf "$dir"
  fi
done

FILES_TO_REMOVE=(
  "progress.log"
  "package.json"
  "package-lock.json"
  "yarn.lock"
  "pnpm-lock.yaml"
  "tsconfig.json"
  "tsconfig.node.json"
  "vite.config.ts"
  "vitest.config.ts"
  ".eslintrc.json"
  "eslint.config.js"
  "eslint.config.mjs"
  "eslint.config.cjs"
  ".prettierrc"
  ".prettierrc.json"
  ".prettierrc.yml"
  ".prettierrc.yaml"
  ".prettierrc.js"
  "index.html"
  "test-browser.js"
)

for file in "${FILES_TO_REMOVE[@]}"; do
  if [ -e "$file" ]; then
    echo "[cleanup] Removing file: $file"
    rm -f "$file"
  fi
done

find "$SCRIPT_DIR" -maxdepth 1 -type f \
  \( -name '*.js' -o -name '*.mjs' -o -name '*.cjs' -o -name '*.jsx' -o -name '*.tsx' \) \
  -print -delete 2>/dev/null || true

mkdir -p snapshots

echo "[cleanup] Done. Preserved INSTRUCTIONS.md, SYSTEMG_UI.md, cleanup.sh."
echo "[cleanup] Workspace reset complete."
