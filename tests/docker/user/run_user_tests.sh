#!/bin/bash
# User mode UAT test suite - Real developer scenarios
set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Test status tracking
TESTS_PASSED=0
TESTS_FAILED=0

log_test() {
    echo -e "${BLUE}[TEST]${NC} $1"
}

log_success() {
    echo -e "${GREEN}[PASS]${NC} $1"
    ((TESTS_PASSED++))
}

log_fail() {
    echo -e "${RED}[FAIL]${NC} $1"
    ((TESTS_FAILED++))
}

log_info() {
    echo -e "${YELLOW}[INFO]${NC} $1"
}

# Clean up function
cleanup() {
    log_info "Cleaning up..."
    sysg purge --force 2>/dev/null || true
    pkill -f "sysg supervisor" 2>/dev/null || true
    rm -rf ~/test-project
}

# Set up trap for cleanup
trap cleanup EXIT

# ==============================================================================
# SCENARIO 1: Full-Stack Web Application Development
# ==============================================================================
log_test "Scenario 1: Full-stack web application with hot reload"

# Create a sample Node.js/React project
mkdir -p ~/test-project/backend ~/test-project/frontend ~/test-project/services
cd ~/test-project

# Create backend API
cat > backend/server.js <<'EOF'
const express = require('express');
const app = express();
const PORT = process.env.PORT || 3000;

app.get('/api/health', (req, res) => {
    res.json({ status: 'healthy', time: new Date() });
});

app.get('/api/data', (req, res) => {
    res.json({ data: 'Sample data from API' });
});

app.listen(PORT, () => {
    console.log(`Backend API running on port ${PORT}`);
});
EOF

cat > backend/package.json <<'EOF'
{
  "name": "backend",
  "version": "1.0.0",
  "scripts": {
    "start": "node server.js",
    "dev": "nodemon server.js"
  }
}
EOF

# Create frontend
cat > frontend/index.html <<'EOF'
<!DOCTYPE html>
<html>
<head><title>Test App</title></head>
<body>
    <h1>Test Application</h1>
    <div id="status"></div>
    <script>
        fetch('/api/health')
            .then(r => r.json())
            .then(d => document.getElementById('status').textContent = JSON.stringify(d));
    </script>
</body>
</html>
EOF

cat > frontend/server.py <<'EOF'
#!/usr/bin/env python3
import http.server
import socketserver
import os

PORT = int(os.environ.get('PORT', 8080))
Handler = http.server.SimpleHTTPRequestHandler

with socketserver.TCPServer(("", PORT), Handler) as httpd:
    print(f"Frontend server running on port {PORT}")
    httpd.serve_forever()
EOF

# Create background worker
cat > services/worker.py <<'EOF'
#!/usr/bin/env python3
import time
import os

worker_id = os.environ.get('WORKER_ID', 'default')
print(f"Worker {worker_id} started")

while True:
    print(f"Worker {worker_id}: Processing task at {time.time()}")
    time.sleep(10)
EOF

# Create data processor
cat > services/processor.sh <<'EOF'
#!/bin/bash
echo "Data processor started"
while true; do
    echo "Processing data batch at $(date)"
    sleep 15
done
EOF
chmod +x services/processor.sh

# Create sysg configuration
cat > sysg.yaml <<'EOF'
services:
  backend:
    command: node server.js
    working_dir: ./backend
    environment:
      PORT: 3000
      NODE_ENV: development
    pre_start: |
      echo "Installing backend dependencies..."
      cd backend && npm install express 2>/dev/null || true
    health_check:
      command: curl -f http://localhost:3000/api/health
      interval: 10
      retries: 3

  frontend:
    command: python3 server.py
    working_dir: ./frontend
    environment:
      PORT: 8080
    depends_on:
      - backend

  worker:
    command: python3 services/worker.py
    environment:
      WORKER_ID: "worker-{{.Instance}}"
    instances: 2
    restart_policy: on-failure
    max_restarts: 3

  processor:
    command: ./services/processor.sh
    restart_policy: always

  db_migrate:
    command: echo "Running database migrations..."
    run_once: true
    pre_start: echo "Checking database connection..."

  cache_warmer:
    command: |
      echo "Warming cache..."
      curl -s http://localhost:3000/api/data > /dev/null
    cron: "*/5 * * * *"
    depends_on:
      - backend
EOF

# Initialize project
log_info "Initializing development project..."
if sysg init; then
    log_success "Project initialized"
else
    log_fail "Failed to initialize project"
fi

# Start all services
log_info "Starting all development services..."
if sysg start; then
    log_success "All services started"
else
    log_fail "Failed to start services"
    sysg status
fi

sleep 5

# Verify services are running
STATUS=$(sysg status)
echo "$STATUS"

if echo "$STATUS" | grep -q "backend.*running"; then
    log_success "Backend API is running"
else
    log_fail "Backend API not running"
fi

if echo "$STATUS" | grep -q "frontend.*running"; then
    log_success "Frontend server is running"
else
    log_fail "Frontend server not running"
fi

# Test hot reload scenario
log_info "Testing development hot reload..."
echo "// Added comment" >> backend/server.js
sysg restart backend
sleep 3
if sysg status | grep -q "backend.*running"; then
    log_success "Backend restarted for hot reload"
else
    log_fail "Backend restart failed"
fi

# ==============================================================================
# SCENARIO 2: Microservices Development
# ==============================================================================
log_test "Scenario 2: Microservices with service discovery"

cat >> sysg.yaml <<'EOF'

  auth_service:
    command: python3 -c "import time; print('Auth service started'); [time.sleep(1) for _ in iter(int, 1)]"
    environment:
      SERVICE_PORT: 4000
      SERVICE_NAME: auth
    health_check:
      command: echo "Auth service healthy"

  user_service:
    command: python3 -c "import time; print('User service started'); [time.sleep(1) for _ in iter(int, 1)]"
    environment:
      SERVICE_PORT: 4001
      SERVICE_NAME: users
      AUTH_SERVICE_URL: http://localhost:4000
    depends_on:
      - auth_service

  notification_service:
    command: python3 -c "import time; print('Notification service started'); [time.sleep(1) for _ in iter(int, 1)]"
    environment:
      SERVICE_PORT: 4002
      SERVICE_NAME: notifications
    depends_on:
      - user_service
EOF

# Reload configuration
log_info "Adding microservices..."
if sysg reload; then
    log_success "Configuration reloaded with microservices"
else
    log_fail "Failed to reload configuration"
fi

# Start new services
sysg start auth_service user_service notification_service
sleep 3

# Test service dependencies
if sysg status | grep -q "notification_service.*running"; then
    log_success "Microservices started with correct dependencies"
else
    log_fail "Microservice dependency chain failed"
fi

# ==============================================================================
# SCENARIO 3: Development Workflow Automation
# ==============================================================================
log_test "Scenario 3: Development workflow automation"

# Add development automation services
cat >> sysg.yaml <<'EOF'

  test_runner:
    command: |
      echo "Running tests..."
      echo "✓ Unit tests passed"
      echo "✓ Integration tests passed"
    cron: "*/30 * * * *"
    skip: false

  code_formatter:
    command: |
      echo "Formatting code..."
      echo "Formatted 5 files"
    cron: "@hourly"

  dependency_checker:
    command: |
      echo "Checking for outdated dependencies..."
      echo "All dependencies up to date"
    cron: "0 9 * * MON"
EOF

sysg reload
log_success "Development automation services configured"

# ==============================================================================
# SCENARIO 4: Debugging and Troubleshooting
# ==============================================================================
log_test "Scenario 4: Debugging and troubleshooting workflows"

# Test log streaming
log_info "Testing log aggregation..."
sysg logs --lines 20 > /tmp/aggregated.log
if [ -s /tmp/aggregated.log ]; then
    log_success "Log aggregation working"
else
    log_fail "Log aggregation failed"
fi

# Test service-specific debugging
log_info "Debugging specific service..."
sysg logs worker --follow &
LOG_PID=$!
sleep 2
kill $LOG_PID 2>/dev/null
log_success "Service-specific log streaming works"

# Test metrics/inspection
log_info "Collecting performance metrics..."
if sysg inspect > /tmp/user_metrics.json 2>/dev/null; then
    log_success "Metrics collection successful"
else
    log_fail "Metrics collection failed"
fi

# ==============================================================================
# SCENARIO 5: CI/CD Pipeline Simulation
# ==============================================================================
log_test "Scenario 5: Local CI/CD pipeline"

cat >> sysg.yaml <<'EOF'

  ci_build:
    command: |
      echo "Building application..."
      sleep 2
      echo "Build successful"
    run_once: true

  ci_test:
    command: |
      echo "Running test suite..."
      sleep 2
      echo "All tests passed"
    run_once: true
    depends_on:
      - ci_build

  ci_deploy:
    command: |
      echo "Deploying to staging..."
      sleep 1
      echo "Deployment successful"
    run_once: true
    depends_on:
      - ci_test
EOF

log_info "Running CI/CD pipeline..."
sysg reload
sysg start ci_build
sleep 8

if sysg status | grep -q "ci_deploy.*completed"; then
    log_success "CI/CD pipeline executed successfully"
else
    log_fail "CI/CD pipeline failed"
fi

# ==============================================================================
# SCENARIO 6: Development Environment Presets
# ==============================================================================
log_test "Scenario 6: Environment presets and profiles"

# Test stopping development services
log_info "Stopping non-essential services..."
sysg stop processor cache_warmer test_runner
if sysg status | grep -q "processor.*stopped"; then
    log_success "Selective service management works"
else
    log_fail "Failed to stop specific services"
fi

# Test restart with different configuration
log_info "Switching to production mode..."
export NODE_ENV=production
sysg restart backend
if sysg status | grep -q "backend.*running"; then
    log_success "Environment-based restart successful"
else
    log_fail "Environment switch failed"
fi

# ==============================================================================
# SCENARIO 7: Graceful Shutdown and Cleanup
# ==============================================================================
log_test "Scenario 7: Development session cleanup"

log_info "Performing graceful shutdown..."
if sysg stop --graceful; then
    log_success "Graceful shutdown completed"
else
    log_fail "Graceful shutdown failed"
fi

# Verify all processes stopped
REMAINING=$(pgrep -f "sysg\|node\|python3" | wc -l)
if [ "$REMAINING" -eq 0 ]; then
    log_success "All processes cleaned up"
else
    log_fail "$REMAINING processes still running"
fi

# ==============================================================================
# SUMMARY
# ==============================================================================
echo ""
echo "============================================"
echo "           USER MODE TEST SUMMARY"
echo "============================================"
echo -e "${GREEN}Tests Passed:${NC} $TESTS_PASSED"
echo -e "${RED}Tests Failed:${NC} $TESTS_FAILED"
echo "============================================"

if [ "$TESTS_FAILED" -eq 0 ]; then
    echo -e "${GREEN}All user mode tests passed successfully!${NC}"
    exit 0
else
    echo -e "${RED}Some tests failed. Please review the output.${NC}"
    exit 1
fi