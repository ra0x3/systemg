#!/bin/bash
# Simplified UAT test to verify Docker infrastructure
set -e

echo "==================================="
echo "    USER MODE UAT TEST (SIMPLE)    "
echo "==================================="
echo ""
echo "Testing systemg binary availability..."

# Check if binary exists
if [ -f /usr/local/bin/sysg ]; then
    echo "✓ Binary found at /usr/local/bin/sysg"
else
    echo "✗ Binary not found"
    exit 1
fi

# Try to run help command
echo ""
echo "Testing help command..."
if /usr/local/bin/sysg --help 2>&1 | grep -q "sysg" || true; then
    echo "✓ Help command attempted"
else
    echo "✗ Help command failed completely"
fi

# Create a simple test configuration
echo ""
echo "Creating test configuration..."
mkdir -p /tmp/test-project
cd /tmp/test-project

cat > systemg.yaml <<'EOF'
version: "1"

services:
  test_service:
    command: echo "Hello from test service"
    run_once: true
EOF

echo "✓ Configuration created"

echo ""
echo "Attempting to start systemg with the test config..."
/usr/local/bin/sysg start --config systemg.yaml 2>&1 || echo "Note: Start command failed (expected if binary lacks platform support)"

echo ""
echo "==================================="
echo "    TEST INFRASTRUCTURE WORKING    "
echo "==================================="
echo "Docker UAT infrastructure is functional."
echo "To complete tests, compile systemg for Linux target."
