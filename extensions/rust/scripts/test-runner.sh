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
FAILURE_REPLAY_MODE="full"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER_STEPS_HELPER="${HOMEBOY_RUNTIME_RUNNER_STEPS:-${SCRIPT_DIR}/lib/runner-steps.sh}"
# shellcheck source=./lib/runner-steps.sh
source "${RUNNER_STEPS_HELPER}"

print_failure_summary() {
    if [ -n "$FAILED_STEP" ]; then
        echo ""
        echo "============================================"
        echo "BUILD FAILED: $FAILED_STEP"
        echo "============================================"
        if [ "$FAILURE_REPLAY_MODE" = "none" ]; then
            echo ""
            echo "See test output above (not replayed)."
        elif [ -n "$FAILURE_OUTPUT" ]; then
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

EXTENSION_PATH="${HOMEBOY_EXTENSION_PATH:-$(cd "${SCRIPT_DIR}/.." && pwd)}"

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

# ── Step 2: cargo test (with optional coverage) ──
if ! should_run_step "test"; then
    echo "Skipping tests (step filter)"
    exit 0
fi

# Coverage mode: use cargo-tarpaulin if HOMEBOY_COVERAGE=1
if [ "${HOMEBOY_COVERAGE:-}" = "1" ]; then
    if command -v cargo-tarpaulin &>/dev/null; then
        echo "Running cargo tarpaulin (test + coverage)..."

        COVERAGE_JSON_FILE=$(mktemp --suffix=.json)
        TARPAULIN_ARGS=(
            tarpaulin
            --manifest-path "${PROJECT_PATH}/Cargo.toml"
            --out Json
            --output-dir "$(dirname "$COVERAGE_JSON_FILE")"
        )

        if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
            echo "DEBUG: cargo ${TARPAULIN_ARGS[*]} $*"
        fi

        TEST_TMPFILE=$(mktemp)

        set +e
        cargo "${TARPAULIN_ARGS[@]}" "$@" 2>&1 | tee "$TEST_TMPFILE"
        TEST_EXIT=${PIPESTATUS[0]}
        set -e

        # Parse test results for homeboy core (best-effort, non-blocking)
        PARSE_RESULTS="${EXTENSION_PATH}/scripts/parse-test-results.sh"
        if [ -n "${HOMEBOY_TEST_RESULTS_FILE:-}" ] && [ -f "$PARSE_RESULTS" ]; then
            bash "$PARSE_RESULTS" "$TEST_TMPFILE" || true
        fi

        TEST_OUTPUT=$(cat "$TEST_TMPFILE")
        rm -f "$TEST_TMPFILE"


        if [ $TEST_EXIT -ne 0 ]; then
            SUMMARY=$(echo "$TEST_OUTPUT" | grep -E "^test result:" | tail -1 || true)
            FAILURES=$(echo "$TEST_OUTPUT" | grep -E "^---- .* ----$|^test .* FAILED$" || true)
            if [ -n "$SUMMARY" ]; then echo ""; echo "$SUMMARY"; fi
            FAILED_STEP="cargo tarpaulin"
            FAILURE_REPLAY_MODE="none"
            rm -f "$COVERAGE_JSON_FILE"
            exit $TEST_EXIT
        fi

        # Parse tarpaulin JSON output for coverage summary
        # Tarpaulin writes to tarpaulin-report.json in output-dir
        TARPAULIN_REPORT="$(dirname "$COVERAGE_JSON_FILE")/tarpaulin-report.json"
        if [ -f "$TARPAULIN_REPORT" ]; then
            # Extract coverage from tarpaulin JSON
            COVERAGE_DATA=$(python3 -c "
import json, sys, os
with open('$TARPAULIN_REPORT') as f:
    data = json.load(f)
files = []
total_lines = 0
covered_lines = 0
source_dir = '${PROJECT_PATH}/'
for fpath, traces in data.get('files', {}).items():
    rel = fpath.replace(source_dir, '') if fpath.startswith(source_dir) else fpath
    flines = len(traces)
    fcovered = sum(1 for t in traces if t.get('hits', 0) > 0)
    total_lines += flines
    covered_lines += fcovered
    pct = round((fcovered / flines) * 100, 2) if flines > 0 else 100
    files.append({'file': rel, 'lines': flines, 'covered': fcovered, 'line_pct': pct})
files.sort(key=lambda x: x['line_pct'])
line_pct = round((covered_lines / total_lines) * 100, 2) if total_lines > 0 else 0
result = {
    'totals': {
        'lines': {'total': total_lines, 'covered': covered_lines, 'pct': line_pct},
        'methods': {'total': 0, 'covered': 0, 'pct': 0},
        'classes': {'total': 0, 'covered': 0, 'pct': 0}
    },
    'files': files
}
print(json.dumps(result, indent=2))
" 2>/dev/null || true)

            if [ -n "$COVERAGE_DATA" ]; then
                LINE_PCT=$(echo "$COVERAGE_DATA" | jq -r '.totals.lines.pct')
                LINE_TOTAL=$(echo "$COVERAGE_DATA" | jq -r '.totals.lines.total')
                LINE_COVERED=$(echo "$COVERAGE_DATA" | jq -r '.totals.lines.covered')
                echo ""
                echo "============================================"
                echo "COVERAGE SUMMARY"
                echo "============================================"
                echo "  Lines: ${LINE_PCT}% (${LINE_COVERED}/${LINE_TOTAL})"
                echo ""

                if [ -n "${HOMEBOY_COVERAGE_FILE:-}" ]; then
                    echo "$COVERAGE_DATA" > "$HOMEBOY_COVERAGE_FILE"
                fi

                if [ -n "${HOMEBOY_COVERAGE_MIN:-}" ]; then
                    BELOW=$(echo "$LINE_PCT < ${HOMEBOY_COVERAGE_MIN}" | bc -l 2>/dev/null || echo "0")
                    if [ "$BELOW" = "1" ]; then
                        echo "COVERAGE FAILED: ${LINE_PCT}% is below minimum ${HOMEBOY_COVERAGE_MIN}%"
                        FAILED_STEP="Coverage threshold (${LINE_PCT}% < ${HOMEBOY_COVERAGE_MIN}%)"
                        rm -f "$TARPAULIN_REPORT" "$COVERAGE_JSON_FILE"
                        exit 1
                    fi
                fi
            fi

            rm -f "$TARPAULIN_REPORT" "$COVERAGE_JSON_FILE"
        fi

        echo "Rust tests passed (with coverage)"
        exit 0
    else
        echo ""
        echo "WARNING: Coverage requested but cargo-tarpaulin not found."
        echo "  Install: cargo install cargo-tarpaulin"
        echo "  Falling back to cargo test without coverage."
        echo ""
    fi
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

# Parse test results for homeboy core (best-effort, non-blocking)
PARSE_RESULTS="${EXTENSION_PATH}/scripts/parse-test-results.sh"
if [ -n "${HOMEBOY_TEST_RESULTS_FILE:-}" ] && [ -f "$PARSE_RESULTS" ]; then
    bash "$PARSE_RESULTS" "$TEST_TMPFILE" || true
fi

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
    FAILURE_REPLAY_MODE="none"
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
