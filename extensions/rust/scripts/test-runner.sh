#!/usr/bin/env bash
set -euo pipefail

# Rust test runner for homeboy test.
#
# Runs cargo test with standard homeboy extension env vars:
#   HOMEBOY_EXTENSION_PATH  — path to this extension
#   HOMEBOY_COMPONENT_PATH  — path to the Rust project
#   HOMEBOY_SKIP_LINT       — if "1", skip the pre-test lint step
#   HOMEBOY_AUTO_FIX        — if "1", auto-fix before testing
#   HOMEBOY_STEP            — comma-separated steps to run (lint, test)
#   HOMEBOY_SKIP            — comma-separated steps to skip
#   HOMEBOY_DEBUG           — if "1", show debug output
#
# Passthrough args after -- are forwarded to cargo test.

FAILED_STEP=""
FAILURE_OUTPUT=""

# Step filtering
should_run_step() {
    local step_name="$1"
    if [ -n "${HOMEBOY_STEP:-}" ]; then
        echo ",${HOMEBOY_STEP}," | grep -q ",${step_name}," && return 0 || return 1
    fi
    if [ -n "${HOMEBOY_SKIP:-}" ]; then
        echo ",${HOMEBOY_SKIP}," | grep -q ",${step_name}," && return 1 || return 0
    fi
    return 0
}

print_failure_summary() {
    if [ -n "$FAILED_STEP" ]; then
        echo ""
        echo "============================================"
        echo "BUILD FAILED: $FAILED_STEP"
        echo "============================================"
        if [ -n "$FAILURE_OUTPUT" ]; then
            echo ""
            echo "Error details:"
            echo "$FAILURE_OUTPUT"
        fi
    fi
}
trap print_failure_summary EXIT

# Determine project path
if [ -n "${HOMEBOY_COMPONENT_PATH:-}" ]; then
    PROJECT_PATH="${HOMEBOY_COMPONENT_PATH}"
else
    PROJECT_PATH="$(pwd)"
fi

EXTENSION_PATH="${HOMEBOY_EXTENSION_PATH:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: Rust Test Environment:"
    echo "HOMEBOY_EXTENSION_PATH=${HOMEBOY_EXTENSION_PATH:-NOT_SET}"
    echo "HOMEBOY_COMPONENT_PATH=${HOMEBOY_COMPONENT_PATH:-NOT_SET}"
    echo "HOMEBOY_SKIP_LINT=${HOMEBOY_SKIP_LINT:-NOT_SET}"
    echo "HOMEBOY_AUTO_FIX=${HOMEBOY_AUTO_FIX:-NOT_SET}"
    echo "PROJECT_PATH=${PROJECT_PATH}"
    echo "Passthrough args: $*"
fi

# Verify this is a Rust project
if [ ! -f "${PROJECT_PATH}/Cargo.toml" ]; then
    echo "Error: No Cargo.toml found at ${PROJECT_PATH}"
    echo "Not a Rust project — cannot run tests."
    exit 1
fi

echo "Running Rust tests..."

# ── Step 1: Pre-test lint (unless skipped) ──
if should_run_step "lint" && [ "${HOMEBOY_SKIP_LINT:-}" != "1" ]; then
    LINT_RUNNER="${EXTENSION_PATH}/scripts/lint-runner.sh"
    if [ -f "$LINT_RUNNER" ]; then
        echo ""
        echo "Running pre-test lint checks..."
        HOMEBOY_SUMMARY_MODE=1 bash "$LINT_RUNNER"
        echo ""
    fi
elif ! should_run_step "lint"; then
    echo "Skipping lint (step filter)"
else
    echo "Skipping lint (--skip-lint)"
fi

# ── Step 2: cargo test ──
if ! should_run_step "test"; then
    echo "Skipping tests (step filter)"
    exit 0
fi

echo "Running cargo test..."

TEST_ARGS=(
    test
    --manifest-path "${PROJECT_PATH}/Cargo.toml"
)

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: cargo ${TEST_ARGS[*]} $*"
fi

TEST_TMPFILE=$(mktemp)

set +e
cargo "${TEST_ARGS[@]}" "$@" 2>&1 | tee "$TEST_TMPFILE"
TEST_EXIT=${PIPESTATUS[0]}
set -e

TEST_OUTPUT=$(cat "$TEST_TMPFILE")
rm -f "$TEST_TMPFILE"

if [ $TEST_EXIT -eq 0 ]; then
    # Extract test summary line
    SUMMARY=$(echo "$TEST_OUTPUT" | grep -E "^test result:" | tail -1 || true)
    if [ -n "$SUMMARY" ]; then
        echo ""
        echo "$SUMMARY"
    fi
    echo ""
    echo "Rust tests passed"
else
    # Extract failure details
    SUMMARY=$(echo "$TEST_OUTPUT" | grep -E "^test result:" | tail -1 || true)
    FAILURES=$(echo "$TEST_OUTPUT" | grep -E "^---- .* ----$|^test .* FAILED$" || true)

    if [ -n "$SUMMARY" ]; then
        echo ""
        echo "$SUMMARY"
    fi

    FAILED_STEP="cargo test"
    FAILURE_OUTPUT="$(echo "$FAILURES" | head -20)"
    exit $TEST_EXIT
fi

# Detect zero-test runs — only warn if NO test result line shows passed tests.
# Cargo runs multiple test binaries (unit, integration, doc-tests); some may
# legitimately have 0 tests while others have hundreds.
TOTAL_PASSED=$(echo "$TEST_OUTPUT" | grep -oP '\d+ passed' | awk '{s+=$1} END {print s+0}')
if [ "$TOTAL_PASSED" -eq 0 ]; then
    TEST_FILE_COUNT=$(find "$PROJECT_PATH" -name "*test*" -name "*.rs" -not -path "*/target/*" 2>/dev/null | wc -l | tr -d ' ')
    if [ "$TEST_FILE_COUNT" -gt 0 ]; then
        echo ""
        echo "============================================"
        echo "WARNING: cargo test ran 0 tests"
        echo "============================================"
        echo ""
        echo "Found ${TEST_FILE_COUNT} test files but no tests were executed."
        echo "This may indicate a configuration issue."
    fi
fi
