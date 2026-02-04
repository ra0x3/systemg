#!/bin/bash

<<<<<<< Updated upstream
# UI Cleanup Script - Reset to starting point for agent restart
# Master, this script shall cleanse the workspace while preserving vital components

set -e

echo "Starting UI cleanup..."

# Remove snapshots
if [ -d "snapshots" ]; then
    echo "Removing snapshots directory..."
    rm -rf snapshots
fi

# Clean build artifacts
if [ -d "dist" ]; then
    echo "Removing dist directory..."
    rm -rf dist
fi

if [ -d "build" ]; then
    echo "Removing build directory..."
    rm -rf build
fi

# Clean node modules (optional - uncomment if needed)
# if [ -d "node_modules" ]; then
#     echo "Removing node_modules..."
#     rm -rf node_modules
# fi

# Clean temporary and cache directories
echo "Cleaning temporary files..."
=======
# UI Cleanup Script - Complete reset for SystemG re-run
# Master, this script shall cleanse the workspace entirely

set -e

echo "Starting COMPLETE UI cleanup..."
echo "⚠️  WARNING: This will remove ALL source code and build artifacts!"
echo ""

# Remove ALL source code
echo "Removing all source code..."
rm -rf src/ 2>/dev/null || true
rm -rf tests/ 2>/dev/null || true
rm -rf public/ 2>/dev/null || true

# Remove ALL build artifacts and dependencies
echo "Removing build artifacts and dependencies..."
rm -rf dist/ 2>/dev/null || true
rm -rf build/ 2>/dev/null || true
rm -rf node_modules/ 2>/dev/null || true
rm -rf coverage/ 2>/dev/null || true

# Remove ALL configuration files (preserving only .env)
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
rm -f .prettierrc 2>/dev/null || true
rm -f index.html 2>/dev/null || true
rm -f test-browser.js 2>/dev/null || true

# Remove ALL temporary and cache directories (preserving .claude)
echo "Removing temporary files and caches..."
>>>>>>> Stashed changes
rm -rf .next 2>/dev/null || true
rm -rf .cache 2>/dev/null || true
rm -rf .turbo 2>/dev/null || true
rm -rf .parcel-cache 2>/dev/null || true
<<<<<<< Updated upstream
rm -rf coverage 2>/dev/null || true

# Clean log files but preserve the logs directory structure
if [ -d "logs" ]; then
    echo "Cleaning log files..."
    find logs -type f -name "*.log" -delete 2>/dev/null || true
fi

# Remove generated files
rm -f *.log 2>/dev/null || true
rm -f *.pid 2>/dev/null || true
rm -f .DS_Store 2>/dev/null || true
rm -rf **/.DS_Store 2>/dev/null || true

# Clean package lock files (optional - uncomment if needed)
# rm -f package-lock.json yarn.lock pnpm-lock.yaml 2>/dev/null || true

echo "Cleanup complete!"
echo ""
echo "Preserved:"
echo "  - instructions/"
echo "  - .env files"
echo "  - source code"
echo "  - package.json"
echo ""
echo "Ready for agent restart, Master!"
=======

# Remove ALL log files
echo "Removing all log files..."
rm -f *.log 2>/dev/null || true
rm -f *.pid 2>/dev/null || true
rm -rf logs/ 2>/dev/null || true

# Remove other generated files
rm -f .DS_Store 2>/dev/null || true
rm -rf **/.DS_Store 2>/dev/null || true

# Reset snapshots directory - keep structure but reset content
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
echo "Master, the workspace has been completely cleansed! Ready for SystemG to rebuild from scratch."
>>>>>>> Stashed changes
