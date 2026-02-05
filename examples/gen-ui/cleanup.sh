#!/bin/bash

set -e

echo "Starting UI cleanup..."

if [ -d "snapshots" ]; then
    echo "Removing snapshots directory..."
    rm -rf snapshots
fi

if [ -d "dist" ]; then
    echo "Removing dist directory..."
    rm -rf dist
fi

if [ -d "build" ]; then
    echo "Removing build directory..."
    rm -rf build
fi

echo "Cleaning temporary files..."

set -e

echo "Starting COMPLETE UI cleanup..."
echo "⚠️  WARNING: This will remove ALL source code and build artifacts!"
echo ""

echo "Removing all source code..."
rm -rf src/ 2>/dev/null || true
rm -rf tests/ 2>/dev/null || true
rm -rf public/ 2>/dev/null || true

echo "Removing build artifacts and dependencies..."
rm -rf dist/ 2>/dev/null || true
rm -rf build/ 2>/dev/null || true
rm -rf node_modules/ 2>/dev/null || true
rm -rf coverage/ 2>/dev/null || true

echo "Removing configuration files..."
rm -f package.json 2>/dev/null || true
rm -f package-lock.json 2>/dev/null || true
rm -f yarn.lock 2>/dev/null || true
rm -f pnpm-lock.yaml 2>/dev/null || true
rm -f tsconfig.json 2>/dev/null || true
rm -f tsconfig.node.json 2>/dev/null || true
rm -f vite.config.ts 2>/dev/null || true
rm -f vitest.config.ts 2>/dev/null || true
rm -f .eslintrc.json 2>/dev/null || true
rm -f eslint.config.js 2>/dev/null || true
rm -f eslint.config.mjs 2>/dev/null || true
rm -f eslint.config.cjs 2>/dev/null || true
rm -f .prettierrc 2>/dev/null || true
rm -f index.html 2>/dev/null || true
rm -f test-browser.js 2>/dev/null || true

echo "Removing temporary files and caches..."
echo "Removing all JavaScript files..."
rm -f *.js 2>/dev/null || true
rm -f *.mjs 2>/dev/null || true
rm -f *.cjs 2>/dev/null || true
rm -f **/*.js 2>/dev/null || true
rm -f **/*.mjs 2>/dev/null || true
rm -f **/*.cjs 2>/dev/null || true

echo "Removing temporary files and caches..."
rm -rf .next 2>/dev/null || true
rm -rf .cache 2>/dev/null || true
rm -rf .turbo 2>/dev/null || true
rm -rf .parcel-cache 2>/dev/null || true
rm -rf coverage 2>/dev/null || true

if [ -d "logs" ]; then
    echo "Cleaning log files..."
    find logs -type f -name "*.log" -delete 2>/dev/null || true
fi

rm -f *.log 2>/dev/null || true
rm -f *.pid 2>/dev/null || true
rm -f .DS_Store 2>/dev/null || true
rm -rf **/.DS_Store 2>/dev/null || true

echo "Cleanup complete!"
echo ""
echo "Preserved:"
echo "  - INSTRUCTIONS.md"
echo "  - SYSTEMG_UI.md"
echo "  - .env files"
echo "  - source code"
echo "  - package.json"
echo ""
echo "Ready for agent restart, Master!"

echo "Removing all log files..."
rm -f *.log 2>/dev/null || true
rm -f *.pid 2>/dev/null || true
rm -rf logs/ 2>/dev/null || true

rm -f .DS_Store 2>/dev/null || true
rm -rf **/.DS_Store 2>/dev/null || true

echo "Resetting snapshots..."
if [ -d "snapshots" ]; then
    rm -rf snapshots/*
    rm -rf snapshots/.*[!.] 2>/dev/null || true

else
    mkdir -p snapshots
fi

echo ""
echo "✅ Complete cleanup finished!"
echo ""
echo "Removed:"
echo "  - ALL source code (src/, tests/, public/)"
echo "  - ALL build artifacts (dist/, node_modules/)"
echo "  - ALL configuration files (except .env)"
echo "  - ALL ESLint configuration files"
echo "  - ALL JavaScript files (*.js, *.mjs, *.cjs)"
echo "  - ALL logs and temporary files"
echo ""
echo "Reset:"
echo "  - snapshots/ (empty files maintained for structure)"
echo ""
echo "Preserved:"
echo "  - .claude/"
echo "  - .env"
echo "  - cleanup.sh (this script)"
echo "  - snapshots/"
echo ""
