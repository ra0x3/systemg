#!/bin/bash
# CI/CD Docker-based UAT testing script for systemg
set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Configuration
COMPOSE_FILE="${COMPOSE_FILE:-docker-compose.yml}"
TEST_MODE="${TEST_MODE:-all}" # Options: all, user, kernel
TEST_TIMEOUT="${TEST_TIMEOUT:-${TIMEOUT:-3600}}" # 60 minutes default test timeout
BUILD_TIMEOUT="${BUILD_TIMEOUT:-3600}" # 60 minutes default build timeout
VERBOSE="${VERBOSE:-0}"

# Logging functions
log_info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

log_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

log_warning() {
    echo -e "${YELLOW}[WARNING]${NC} $1"
}

run_timeout() {
    local seconds=$1
    shift

    if command -v timeout &> /dev/null; then
        timeout "$seconds" "$@"
        return $?
    fi

    if command -v gtimeout &> /dev/null; then
        gtimeout "$seconds" "$@"
        return $?
    fi

    "$@" &
    local command_pid=$!

    (
        sleep "$seconds"
        kill -TERM "$command_pid" 2>/dev/null || true
        sleep 5
        kill -KILL "$command_pid" 2>/dev/null || true
    ) &
    local watchdog_pid=$!

    wait "$command_pid"
    local status=$?

    kill "$watchdog_pid" 2>/dev/null || true
    wait "$watchdog_pid" 2>/dev/null || true

    return "$status"
}

# Cleanup function
cleanup() {
    log_info "Cleaning up Docker containers..."
    docker-compose -f "$COMPOSE_FILE" down -v --remove-orphans 2>/dev/null || true

    # Remove test images if CI environment
    if [ "$CI" = "true" ]; then
        docker rmi systemg-user systemg-kernel 2>/dev/null || true
    fi
}

# Error handler
error_handler() {
    local line_no=$1
    log_error "Test failed at line $line_no"

    # Dump logs for debugging
    if [ "$VERBOSE" = "1" ] || [ "$CI" = "true" ]; then
        log_info "Dumping container logs..."
        docker-compose -f "$COMPOSE_FILE" logs --tail=100
    fi

    cleanup
    exit 1
}

# Set up error handling
trap 'error_handler $LINENO' ERR
trap cleanup EXIT

# Check prerequisites
check_prerequisites() {
    log_info "Checking prerequisites..."

    # Check for Docker
    if ! command -v docker &> /dev/null; then
        log_error "Docker is not installed"
        exit 1
    fi

    # Check for Docker Compose
    if ! command -v docker-compose &> /dev/null; then
        log_error "Docker Compose is not installed"
        exit 1
    fi

    # Check Docker daemon
    if ! docker ps; then
        log_error "Docker daemon is not running"
        exit 1
    fi

    # Check if running as root (warn for kernel mode)
    if [ "$TEST_MODE" = "kernel" ] || [ "$TEST_MODE" = "all" ]; then
        if [ "$EUID" -ne 0 ] && [ "$CI" != "true" ]; then
            log_warning "Kernel mode tests may require root privileges"
        fi
    fi

    log_success "Prerequisites check passed"
}

# Build Docker images
build_images() {
    log_info "Building Docker images..."

    case "$TEST_MODE" in
        user)
            run_timeout "$BUILD_TIMEOUT" docker build -f tests/docker/user/Dockerfile -t systemg-user . || {
                log_error "User-space Docker image build timed out or failed after ${BUILD_TIMEOUT} seconds"
                return 1
            }
            ;;
        kernel)
            run_timeout "$BUILD_TIMEOUT" docker build -f tests/docker/kernel/Dockerfile -t systemg-kernel . || {
                log_error "Kernel-space Docker image build timed out or failed after ${BUILD_TIMEOUT} seconds"
                return 1
            }
            ;;
        all)
            run_timeout "$BUILD_TIMEOUT" docker build -f tests/docker/user/Dockerfile -t systemg-user . || {
                log_error "User-space Docker image build timed out or failed after ${BUILD_TIMEOUT} seconds"
                return 1
            }
            run_timeout "$BUILD_TIMEOUT" docker build -f tests/docker/kernel/Dockerfile -t systemg-kernel . || {
                log_error "Kernel-space Docker image build timed out or failed after ${BUILD_TIMEOUT} seconds"
                return 1
            }
            ;;
    esac

    log_success "Docker images built successfully"
}

# Run user mode tests
run_user_tests() {
    log_info "Starting user mode tests..."

    # Start infrastructure services
    docker-compose -f "$COMPOSE_FILE" up -d test-redis test-postgres

    # Wait for services to be healthy
    log_info "Waiting for test infrastructure..."
    run_timeout 60 bash -c 'until [ "$(docker-compose -f "$1" ps | grep -c healthy)" -ge 2 ]; do sleep 1; done' _ "$COMPOSE_FILE" || {
        log_error "Test infrastructure failed to start"
        return 1
    }

    # Run user mode tests
    if run_timeout "$TEST_TIMEOUT" docker-compose -f "$COMPOSE_FILE" run \
        --rm \
        -T \
        --name systemg-user-test-run \
        systemg-user; then
        log_success "User-space tests passed"
        return 0
    else
        log_error "User-space tests failed or timed out after ${TEST_TIMEOUT} seconds"
        return 1
    fi
}

# Run kernel mode tests
run_kernel_tests() {
    log_info "Starting kernel mode tests..."

    # Start infrastructure services
    docker-compose -f "$COMPOSE_FILE" up -d test-redis test-postgres

    # Wait for services to be healthy
    log_info "Waiting for test infrastructure..."
    run_timeout 60 bash -c 'until [ "$(docker-compose -f "$1" ps | grep -c healthy)" -ge 2 ]; do sleep 1; done' _ "$COMPOSE_FILE" || {
        log_error "Test infrastructure failed to start"
        return 1
    }

    # Run kernel mode tests (requires privileged mode)
    if run_timeout "$TEST_TIMEOUT" docker-compose -f "$COMPOSE_FILE" run \
        --rm \
        -T \
        --name systemg-kernel-test-run \
        systemg-kernel; then
        log_success "Kernel-space tests passed"
        return 0
    else
        log_error "Kernel-space tests failed or timed out after ${TEST_TIMEOUT} seconds"
        return 1
    fi
}

# Run tests with timeout
run_with_timeout() {
    local test_func=$1
    local test_name=$2

    "$test_func" || {
        log_error "$test_name failed"
        return 1
    }
}

# Generate test report
generate_report() {
    local user_result=$1
    local kernel_result=$2
    local output_file="${REPORT_FILE:-test-report.txt}"

    {
        echo "=========================================="
        echo "     systemg Docker UAT Test Report"
        echo "=========================================="
        echo "Date: $(date)"
        echo "Test Mode: $TEST_MODE"
        echo ""

        if [ "$TEST_MODE" = "all" ] || [ "$TEST_MODE" = "user" ]; then
            echo "User-Space Tests: $([ $user_result -eq 0 ] && echo "PASSED" || echo "FAILED")"
        fi

        if [ "$TEST_MODE" = "all" ] || [ "$TEST_MODE" = "kernel" ]; then
            echo "Kernel-Space Tests: $([ $kernel_result -eq 0 ] && echo "PASSED" || echo "FAILED")"
        fi

        echo "=========================================="
    } | tee "$output_file"
}

# Main execution
main() {
    local user_result=0
    local kernel_result=0

    log_info "Starting systemg Docker UAT tests (mode: $TEST_MODE)"

    # Check prerequisites
    check_prerequisites

    # Build images
    build_images

    # Run tests based on mode
    case "$TEST_MODE" in
        user)
            run_with_timeout run_user_tests "User mode tests" || user_result=$?
            ;;
        kernel)
            run_with_timeout run_kernel_tests "Kernel mode tests" || kernel_result=$?
            ;;
        all)
            run_with_timeout run_user_tests "User mode tests" || user_result=$?
            run_with_timeout run_kernel_tests "Kernel mode tests" || kernel_result=$?
            ;;
        *)
            log_error "Invalid test mode: $TEST_MODE (options: all, user, kernel)"
            exit 1
            ;;
    esac

    # Generate report
    generate_report $user_result $kernel_result

    # Determine overall result
    if [ $user_result -ne 0 ] || [ $kernel_result -ne 0 ]; then
        log_error "Some tests failed"
        exit 1
    else
        log_success "All tests passed successfully!"
        exit 0
    fi
}

# Parse command line arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --verbose|-v)
            VERBOSE=1
            shift
            ;;
        --timeout|-t)
            TEST_TIMEOUT="$2"
            shift 2
            ;;
        --build-timeout)
            BUILD_TIMEOUT="$2"
            shift 2
            ;;
        --compose-file|-f)
            COMPOSE_FILE="$2"
            shift 2
            ;;
        --help|-h)
            trap - EXIT
            cat <<EOF
Usage: $0 [OPTIONS] [MODE]

Run systemg Docker-based UAT tests

MODE:
    all     Run both user and kernel mode tests (default)
    user    Run only user mode tests
    kernel  Run only kernel mode tests

OPTIONS:
    -v, --verbose         Enable verbose output
    -t, --timeout SEC     Set test run timeout in seconds (default: 3600)
    --build-timeout SEC   Set Docker image build timeout in seconds (default: 3600)
    -f, --compose-file    Specify docker-compose file
    -h, --help           Show this help message

Environment Variables:
    CI                   Set to 'true' for CI mode
    COMPOSE_FILE         Docker compose file path
    TEST_TIMEOUT         Test run timeout in seconds
    BUILD_TIMEOUT        Docker image build timeout in seconds
    TIMEOUT              Backward-compatible alias for TEST_TIMEOUT
    VERBOSE              Enable verbose output (0 or 1)
    REPORT_FILE          Test report output file

Examples:
    $0                   # Run all tests
    $0 user              # Run only user mode tests
    $0 kernel --verbose  # Run kernel tests with verbose output
    CI=true $0           # Run in CI mode
EOF
            exit 0
            ;;
        *)
            TEST_MODE="$1"
            shift
            ;;
    esac
done

# Run main function
main
