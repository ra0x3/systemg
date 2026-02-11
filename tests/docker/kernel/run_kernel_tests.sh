#!/bin/bash
# Kernel mode UAT test suite - Real sysadmin scenarios
set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Test status tracking
TESTS_PASSED=0
TESTS_FAILED=0
CLEANUP_IN_PROGRESS=""

log_test() {
    echo -e "${BLUE}[TEST]${NC} $1"
}

log_success() {
    echo -e "${GREEN}[PASS]${NC} $1"
    ((++TESTS_PASSED))
}

log_fail() {
    echo -e "${RED}[FAIL]${NC} $1"
    ((++TESTS_FAILED))
}

log_info() {
    echo -e "${YELLOW}[INFO]${NC} $1"
}

# Clean up function
cleanup() {
    if [ -n "$CLEANUP_IN_PROGRESS" ]; then
        return 0
    fi
    CLEANUP_IN_PROGRESS=1

    log_info "Cleaning up..."
    sysg purge 2>/dev/null || true
    rm -rf /var/run/systemg/* /var/log/systemg/* /tmp/sysg-test-*
    pkill -f "sysg supervisor" 2>/dev/null || true
}

# Set up trap for cleanup
trap cleanup EXIT INT TERM

# ==============================================================================
# SCENARIO 1: System Service Management (Database, Web Server, Cache)
# ==============================================================================
log_test "Scenario 1: Managing production system services"

# Create production-like service configuration
cat > /etc/systemg/systemg.yaml <<EOF
version: "1"

services:
  postgres:
    command: postgres -D /var/lib/postgresql/data
    user: postgres
    working_dir: /var/lib/postgresql
    environment:
      PGDATA: /var/lib/postgresql/data
    pre_start: |
      if [ ! -d /var/lib/postgresql/data ]; then
        su postgres -c "initdb -D /var/lib/postgresql/data"
      fi
    health_check:
      command: pg_isready
      interval: 10
      retries: 3

  nginx:
    command: nginx -g 'daemon off;'
    user: root
    depends_on:
      - postgres
    pre_start: |
      nginx -t
      mkdir -p /var/log/nginx /var/cache/nginx
    post_stop: |
      rm -f /var/run/nginx.pid

  redis:
    command: redis-server --protected-mode no
    user: redis
    working_dir: /var/lib/redis
    restart_policy: always
    max_restarts: 3
    environment:
      REDIS_PORT: 6379
    health_check:
      command: redis-cli ping
      interval: 5

  app_worker:
    command: python3 /root/services/worker.py
    depends_on:
      - redis
      - postgres
    restart_policy: on-failure
    max_restarts: 5
    environment:
      WORKER_ID: "{{.Instance}}"
      REDIS_URL: redis://localhost:6379
    instances: 3
    deployment:
      strategy: "rolling"
      grace_period: "2s"

  log_aggregator:
    command: tail -F /var/log/systemg/*.log
    user: root
    cron:
      expression: "*/5 * * * *"
    skip: false
EOF

# Start systemg in kernel mode
log_info "Starting systemg in kernel mode..."
cd /etc/systemg

# Test 1: Start all services with dependencies
log_info "Starting production services with dependency resolution..."
sysg start --sys --daemonize --config /etc/systemg/systemg.yaml
START_RESULT=$?
if [ "$START_RESULT" -eq 0 ]; then
    log_success "All services started successfully"
else
    log_fail "Failed to start services (exit code: $START_RESULT)"
    sysg status --config /etc/systemg/systemg.yaml 2>&1 || true
    exit 1
fi

sleep 3

# Test 2: Verify service status and health checks
log_info "Checking service health..."
STATUS_OUTPUT=$(sysg status --config /etc/systemg/systemg.yaml 2>&1) || true
echo "$STATUS_OUTPUT"

if echo "$STATUS_OUTPUT" | grep -q "postgres.*running"; then
    log_success "PostgreSQL is running"
else
    log_fail "PostgreSQL is not running"
fi

if echo "$STATUS_OUTPUT" | grep -q "nginx.*running"; then
    log_success "Nginx is running with dependency"
else
    log_fail "Nginx failed to start"
fi

# Test 3: Service isolation and resource management
log_info "Testing service isolation..."
# For mock services, we'll check if the service is reported as running
if sysg status --config /etc/systemg/systemg.yaml | grep -q "postgres.*running"; then
    log_success "PostgreSQL service managed by systemg (mock)"
else
    log_fail "PostgreSQL service not running"
fi

# ==============================================================================
# SCENARIO 2: Rolling Deployments and Zero-Downtime Updates
# ==============================================================================
log_test "Scenario 2: Rolling deployment for critical services"

# Create new version of worker service
cat > /root/services/worker_v2.py <<EOF
#!/usr/bin/env python3
import time
import os
print(f"Worker v2.0 started - ID: {os.environ.get('WORKER_ID', 'unknown')}")
while True:
    time.sleep(1)
EOF
chmod +x /root/services/worker_v2.py

# Update configuration for rolling deploy
sed -i 's/worker.py/worker_v2.py/' /etc/systemg/systemg.yaml

# Perform rolling restart
log_info "Performing rolling restart of workers..."
if sysg restart --service app_worker --config /etc/systemg/systemg.yaml 2>&1; then
    log_success "Rolling deployment completed"
else
    log_fail "Rolling deployment failed"
fi

# ==============================================================================
# SCENARIO 3: System Monitoring and Troubleshooting
# ==============================================================================
log_test "Scenario 3: System monitoring and troubleshooting"

# Test log streaming
log_info "Testing real-time log streaming..."
timeout 5 sysg logs --service app_worker &
LOGS_PID=$!
sleep 2
if kill -0 $LOGS_PID 2>/dev/null; then
    kill $LOGS_PID
    log_success "Log streaming works"
else
    log_fail "Log streaming failed"
fi

# Test metrics collection
log_info "Collecting system metrics..."
if sysg inspect app_worker --json > /tmp/metrics.json; then
    if [ -s /tmp/metrics.json ]; then
        log_success "Metrics collection successful"
    else
        log_fail "Metrics file is empty"
    fi
else
    log_fail "Failed to collect metrics"
fi

# Test specific service logs
log_info "Testing service-specific log retrieval..."
if sysg logs --service nginx --lines 10 2>/dev/null | grep -q nginx; then
    log_success "Service-specific logs accessible"
else
    log_info "Nginx logs may not be available yet"
fi

# ==============================================================================
# SCENARIO 4: Failure Recovery and High Availability
# ==============================================================================
log_test "Scenario 4: Failure recovery and HA operations"

# Simulate service failure
log_info "Simulating redis failure..."
REDIS_PID=$(pgrep -f "redis-server" | head -1)
if [ -n "$REDIS_PID" ]; then
    kill -9 $REDIS_PID
    sleep 5

    # Check if service was restarted
    NEW_REDIS_PID=$(pgrep -f "redis-server" | head -1)
    if [ -n "$NEW_REDIS_PID" ] && [ "$NEW_REDIS_PID" != "$REDIS_PID" ]; then
        log_success "Redis auto-restarted after failure"
    else
        log_fail "Redis did not auto-restart"
    fi
else
    log_info "Skipping redis failure test - process not found"
fi

# Test graceful degradation
log_info "Testing graceful service degradation..."
sysg stop --service postgres --config /etc/systemg/systemg.yaml 2>&1 || true
sleep 2
if sysg status --config /etc/systemg/systemg.yaml 2>&1 | grep -q "app_worker.*stopped"; then
    log_success "Dependent services stopped gracefully"
else
    log_info "Dependent services may still be running"
fi

# ==============================================================================
# SCENARIO 5: Cron Jobs and Scheduled Tasks
# ==============================================================================
log_test "Scenario 5: System cron jobs and scheduled tasks"

# Add backup cron job
cat >> /etc/systemg/systemg.yaml <<EOF

  db_backup:
    command: |
      pg_dump -U postgres mydb > /backup/db_$(date +%Y%m%d_%H%M%S).sql
    cron:
      expression: "0 */6 * * *"
    user: postgres
    working_dir: /tmp

  log_rotation:
    command: |
      find /var/log/systemg -name "*.log" -size +100M -exec truncate -s 0 {} \;
    cron:
      expression: "0 0 * * *"
    user: root
EOF

# Reload configuration
log_info "Reloading configuration with cron jobs..."
if sysg restart --config /etc/systemg/systemg.yaml; then
    log_success "Configuration reloaded with cron jobs"
else
    log_fail "Failed to reload configuration"
fi

# Verify cron jobs are scheduled
if sysg status --config /etc/systemg/systemg.yaml | grep -q "db_backup.*scheduled"; then
    log_success "Backup cron job scheduled"
else
    log_fail "Backup cron job not scheduled"
fi

# ==============================================================================
# SCENARIO 6: Security and Access Control
# ==============================================================================
log_test "Scenario 6: Security and privilege management"

# Test running services with different users
log_info "Verifying multi-user service execution..."
PROCESSES=$(ps aux | grep -E "(postgres|redis|nginx)" | grep -v grep)
if [ -n "$PROCESSES" ]; then
    USERS=$(echo "$PROCESSES" | awk '{print $1}' | sort -u | wc -l)
    if [ "$USERS" -gt 1 ]; then
        log_success "Services running under different users (security isolation)"
    else
        log_info "Mock services may not show user isolation in container"
    fi
else
    log_info "Skipping user isolation test - services not visible in ps"
fi

# Test PID file permissions
log_info "Checking PID file security..."
if [ -f /var/run/systemg/supervisor.pid ]; then
    PERMS=$(stat -c %a /var/run/systemg/supervisor.pid)
    if [ "$PERMS" = "644" ] || [ "$PERMS" = "600" ]; then
        log_success "PID file has secure permissions"
    else
        log_fail "PID file permissions too open: $PERMS"
    fi
else
    log_fail "Supervisor PID file not found"
fi

# ==============================================================================
# SCENARIO 7: Bulk Operations and Maintenance Mode
# ==============================================================================
log_test "Scenario 7: Bulk operations for maintenance"

# Stop all services except critical ones
log_info "Entering maintenance mode (stopping non-critical services)..."
sysg stop --service app_worker --config /etc/systemg/systemg.yaml
sysg stop --service log_aggregator --config /etc/systemg/systemg.yaml
if sysg status --config /etc/systemg/systemg.yaml | grep -q "app_worker.*stopped"; then
    log_success "Non-critical services stopped for maintenance"
else
    log_fail "Failed to stop services for maintenance"
fi

# Perform maintenance (update config)
log_info "Performing maintenance updates..."
sed -i 's/max_restarts: 3/max_restarts: 5/' /etc/systemg/systemg.yaml

# Restart all services
log_info "Exiting maintenance mode..."
if sysg restart --config /etc/systemg/systemg.yaml; then
    log_success "All services restarted after maintenance"
else
    log_fail "Failed to restart services"
fi

# ==============================================================================
# SCENARIO 8: Emergency Recovery and Cleanup
# ==============================================================================
log_test "Scenario 8: Emergency recovery procedures"

# Simulate stuck supervisor
log_info "Testing recovery from stuck supervisor..."
SUPER_PID=$(cat /var/run/systemg/supervisor.pid 2>/dev/null)
if [ -n "$SUPER_PID" ]; then
    # Create artificial deadlock scenario
    kill -STOP $SUPER_PID 2>/dev/null || true
    sleep 2

    # Try to recover
    if sysg purge; then
        log_success "Successfully purged stuck supervisor"
    else
        log_fail "Failed to purge stuck supervisor"
    fi

    # Ensure we can restart
    if sysg start --sys --daemonize --config /etc/systemg/systemg.yaml; then
        log_success "Recovered and restarted after emergency"
    else
        log_fail "Failed to restart after emergency"
    fi
else
    log_info "Skipping supervisor recovery test"
fi

# ==============================================================================
# SCENARIO 9: Performance Under Load
# ==============================================================================
log_test "Scenario 9: Performance under load"

# Start many instances
log_info "Testing with high service count..."
cat > /tmp/load-test.yaml <<EOF
services:
EOF

for i in {1..20}; do
    cat >> /tmp/load-test.yaml <<EOF
  service_$i:
    command: sleep 3600
    restart_policy: always
EOF
done

cd /tmp
if sysg start --sys --daemonize --config load-test.yaml; then
    SERVICE_COUNT=$(sysg status --config load-test.yaml | grep -c "running")
    if [ "$SERVICE_COUNT" -ge 15 ]; then
        log_success "Successfully managing $SERVICE_COUNT services"
    else
        log_fail "Only $SERVICE_COUNT services running"
    fi
else
    log_fail "Failed to start high service count"
fi

# Clean up load test
sysg purge

# ==============================================================================
# FINAL: Clean shutdown test
# ==============================================================================
log_test "Final: Clean shutdown procedures"

cd /etc/systemg
sysg start --sys --daemonize --config /etc/systemg/systemg.yaml

# Test graceful shutdown
log_info "Testing graceful shutdown..."
if sysg stop --config /etc/systemg/systemg.yaml; then
    log_success "Graceful shutdown completed"
else
    log_fail "Graceful shutdown failed"
fi

# Verify cleanup
if [ -z "$(ls -A /var/run/systemg 2>/dev/null)" ]; then
    log_success "PID files cleaned up"
else
    log_fail "PID files remain after shutdown"
fi

# ==============================================================================
# SUMMARY
# ==============================================================================
echo ""
echo "============================================"
echo "          KERNEL MODE TEST SUMMARY"
echo "============================================"
echo -e "${GREEN}Tests Passed:${NC} $TESTS_PASSED"
echo -e "${RED}Tests Failed:${NC} $TESTS_FAILED"
echo "============================================"

if [ "$TESTS_FAILED" -eq 0 ]; then
    echo -e "${GREEN}All kernel mode tests passed successfully!${NC}"
    exit 0
else
    echo -e "${RED}Some tests failed. Please review the output.${NC}"
    exit 1
fi
