#!/bin/bash
#
# Network Resilience Test Script
#
# Tests notedeck relay connections under degraded network conditions using
# the throttle tool for system-level network simulation.
#
# ARCHITECTURE:
#   This script orchestrates three components:
#   1. strfry - A local Nostr relay for testing
#   2. throttle - Network simulation tool (npm package @sitespeed.io/throttle)
#   3. cargo test - Rust integration tests in crates/enostr/tests/
#
# PREREQUISITES:
#   - Node.js and npm installed
#   - throttle: npm install -g @sitespeed.io/throttle
#   - strfry: compiled and available in $STRFRY_PATH or ../strfry/
#   - sudo access (throttle requires root for network simulation)
#
# USAGE:
#   ./scripts/network-resilience-test.sh [scenario]
#
# SCENARIOS:
#   all        - Run all scenarios (default)
#   baseline   - No throttling (for comparison)
#   3g         - 3G network (1600/768 kbit/s, 150ms RTT)
#   3gslow     - Slow 3G (400/400 kbit/s, 200ms RTT)
#   packetloss - 3G with 10% packet loss
#   disconnect - Simulate intermittent disconnects
#
# ENVIRONMENT VARIABLES:
#   STRFRY_PATH   - Path to strfry directory (default: ../strfry)
#   STRFRY_PORT   - Port for strfry relay (default: 7777)
#   STRFRY_DB     - Database directory (default: /tmp/strfry-test-db)
#   TEST_TIMEOUT  - Test timeout in seconds (default: 120)
#
# EXIT CODES:
#   0 - All tests passed
#   1 - One or more tests failed
#   2 - Prerequisites not met

set -e

# =============================================================================
# Configuration
# =============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
STRFRY_PATH="${STRFRY_PATH:-$PROJECT_ROOT/../strfry}"
STRFRY_PORT="${STRFRY_PORT:-7777}"
STRFRY_DB="${STRFRY_DB:-/tmp/strfry-test-db}"
TEST_TIMEOUT="${TEST_TIMEOUT:-120}"

# Process state (avoid globals by using explicit variable passing)
STRFRY_PID=""

# =============================================================================
# Output Formatting
# =============================================================================

readonly RED='\033[0;31m'
readonly GREEN='\033[0;32m'
readonly YELLOW='\033[1;33m'
readonly BLUE='\033[0;34m'
readonly NC='\033[0m'

log_info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

log_success() {
    echo -e "${GREEN}[PASS]${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

log_error() {
    echo -e "${RED}[FAIL]${NC} $1"
}

# =============================================================================
# Cleanup
# =============================================================================

# Cleanup function - called on script exit
# Uses explicit variable references rather than globals
cleanup() {
    log_info "Cleaning up..."

    # Stop throttle (early return pattern - handle each case independently)
    if command -v throttle &> /dev/null; then
        sudo throttle --stop 2>/dev/null || true
        sudo throttle --stop --localhost 2>/dev/null || true
    fi

    # Stop strfry if running
    if [ -n "$STRFRY_PID" ] && kill -0 "$STRFRY_PID" 2>/dev/null; then
        kill "$STRFRY_PID" 2>/dev/null || true
        wait "$STRFRY_PID" 2>/dev/null || true
    fi

    # Clean up test database
    rm -rf "$STRFRY_DB" 2>/dev/null || true

    log_info "Cleanup complete"
}

trap cleanup EXIT

# =============================================================================
# Prerequisites Validation
# =============================================================================

# Checks that all required tools are available.
# Returns 0 if all prerequisites are met, exits with 2 otherwise.
check_prerequisites() {
    log_info "Checking prerequisites..."

    # Check throttle (early return on failure)
    if ! command -v throttle &> /dev/null; then
        log_error "throttle not found. Install with: npm install -g @sitespeed.io/throttle"
        exit 2
    fi

    # Check strfry (early return on failure)
    if [ ! -x "$STRFRY_PATH/strfry" ]; then
        log_error "strfry not found at $STRFRY_PATH/strfry"
        log_info "Set STRFRY_PATH or compile strfry in ../strfry/"
        exit 2
    fi

    # Check sudo access
    if ! sudo -n true 2>/dev/null; then
        log_warn "sudo access required for network throttling"
        log_info "You may be prompted for your password"
    fi

    # Check Rust/Cargo (early return on failure)
    if ! command -v cargo &> /dev/null; then
        log_error "cargo not found. Install Rust toolchain."
        exit 2
    fi

    log_success "Prerequisites check passed"
    return 0
}

# =============================================================================
# Relay Management
# =============================================================================

# Starts the strfry relay with test configuration.
# Sets STRFRY_PID on success, exits with 1 on failure.
start_strfry() {
    log_info "Starting strfry relay on port $STRFRY_PORT..."

    mkdir -p "$STRFRY_DB"

    # Create minimal strfry config
    cat > "$STRFRY_DB/strfry.conf" << EOF
db = "$STRFRY_DB/strfry-db/"

relay {
    bind = "127.0.0.1"
    port = $STRFRY_PORT

    info {
        name = "Test Relay"
        description = "Notedeck network resilience test relay"
    }
}
EOF

    # Start strfry in background
    cd "$STRFRY_PATH"
    ./strfry --config="$STRFRY_DB/strfry.conf" relay &
    STRFRY_PID=$!
    cd "$PROJECT_ROOT"

    # Wait for strfry to be ready (early return on timeout)
    local max_wait=30
    local waited=0
    while ! nc -z 127.0.0.1 "$STRFRY_PORT" 2>/dev/null; do
        sleep 1
        waited=$((waited + 1))
        if [ $waited -ge $max_wait ]; then
            log_error "strfry failed to start within ${max_wait}s"
            exit 1
        fi
    done

    log_success "strfry started (PID: $STRFRY_PID)"
    return 0
}

# =============================================================================
# Network Throttling
# =============================================================================

# Applies network throttling with the specified profile.
#
# Arguments:
#   $1 - throttle profile name (e.g., "3g", "3gslow")
#   $2 - packet loss percentage (optional, default: 0)
apply_throttle() {
    local profile="$1"
    local packet_loss="${2:-0}"

    log_info "Applying network throttle: $profile (packet loss: ${packet_loss}%)"

    if [ "$packet_loss" -gt 0 ]; then
        sudo throttle "$profile" --packetLoss "$packet_loss" --localhost
    else
        sudo throttle "$profile" --localhost
    fi

    log_success "Throttle applied"
    return 0
}

# Stops all network throttling.
stop_throttle() {
    log_info "Stopping network throttle..."
    sudo throttle --stop --localhost 2>/dev/null || true
    log_success "Throttle stopped"
    return 0
}

# =============================================================================
# Test Execution
# =============================================================================

# Runs the Rust integration tests for the specified scenario.
#
# Arguments:
#   $1 - scenario name
#
# Returns:
#   0 if tests pass, 1 if tests fail
run_tests() {
    local scenario="$1"

    log_info "Running tests for scenario: $scenario"

    cd "$PROJECT_ROOT"

    # Set environment variables for tests
    export TEST_RELAY_URL="ws://127.0.0.1:$STRFRY_PORT"
    export TEST_SCENARIO="$scenario"
    export RUST_LOG="${RUST_LOG:-info}"

    # Run integration tests with timeout
    # Note: We don't fudge tests - if they fail, we report the actual failure
    if timeout "$TEST_TIMEOUT" cargo test \
        --package enostr \
        --test network_resilience \
        -- --test-threads=1 --nocapture 2>&1; then
        log_success "Tests passed for scenario: $scenario"
        return 0
    else
        log_error "Tests failed for scenario: $scenario"
        return 1
    fi
}

# =============================================================================
# Test Scenarios
# Each scenario is self-contained and can be run independently.
# =============================================================================

scenario_baseline() {
    log_info "=== Scenario: Baseline (no throttling) ==="
    run_tests "baseline"
}

scenario_3g() {
    log_info "=== Scenario: 3G Network ==="
    apply_throttle "3g"
    local result=0
    run_tests "3g" || result=1
    stop_throttle
    return $result
}

scenario_3gslow() {
    log_info "=== Scenario: Slow 3G Network ==="
    apply_throttle "3gslow"
    local result=0
    run_tests "3gslow" || result=1
    stop_throttle
    return $result
}

scenario_packetloss() {
    log_info "=== Scenario: 3G with 10% Packet Loss ==="
    apply_throttle "3g" 10
    local result=0
    run_tests "packetloss" || result=1
    stop_throttle
    return $result
}

scenario_disconnect() {
    log_info "=== Scenario: Intermittent Disconnects ==="

    export TEST_RELAY_URL="ws://127.0.0.1:$STRFRY_PORT"
    export TEST_SCENARIO="disconnect"

    # Start the disconnect simulation in background
    # This cycles throttle on/off to simulate network instability
    local disconnect_pid=""
    (
        for i in {1..5}; do
            sleep 3
            log_info "Simulating disconnect $i/5..."
            sudo throttle --rtt 5000 --localhost 2>/dev/null || true
            sleep 2
            sudo throttle --stop --localhost 2>/dev/null || true
        done
    ) &
    disconnect_pid=$!

    local result=0
    run_tests "disconnect" || result=1

    # Clean up disconnect simulation
    if [ -n "$disconnect_pid" ]; then
        kill "$disconnect_pid" 2>/dev/null || true
        wait "$disconnect_pid" 2>/dev/null || true
    fi
    stop_throttle

    return $result
}

# =============================================================================
# Main Entry Point
# =============================================================================

main() {
    local scenario="${1:-all}"
    local failed=0

    echo ""
    echo "========================================"
    echo "  Notedeck Network Resilience Tests"
    echo "========================================"
    echo ""

    check_prerequisites
    start_strfry

    case "$scenario" in
        all)
            scenario_baseline || failed=1
            echo ""
            scenario_3g || failed=1
            echo ""
            scenario_3gslow || failed=1
            echo ""
            scenario_packetloss || failed=1
            echo ""
            scenario_disconnect || failed=1
            ;;
        baseline)
            scenario_baseline || failed=1
            ;;
        3g)
            scenario_3g || failed=1
            ;;
        3gslow)
            scenario_3gslow || failed=1
            ;;
        packetloss)
            scenario_packetloss || failed=1
            ;;
        disconnect)
            scenario_disconnect || failed=1
            ;;
        *)
            log_error "Unknown scenario: $scenario"
            echo "Valid scenarios: all, baseline, 3g, 3gslow, packetloss, disconnect"
            exit 1
            ;;
    esac

    echo ""
    echo "========================================"
    if [ $failed -eq 0 ]; then
        log_success "All tests passed!"
        exit 0
    else
        log_error "Some tests failed"
        exit 1
    fi
}

main "$@"
