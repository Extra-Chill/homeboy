#!/usr/bin/env bash
set -euo pipefail

# Node.js test runner for `homeboy test`.
#
# Runs `npm test` (or pnpm/yarn equivalent) and parses the result count
# into the canonical TestResults JSON envelope homeboy core expects.
#
# Detection order for "what runs":
#   1. HOMEBOY_NODE_TEST_COMMAND env var (full override)
#   2. package.json scripts.test (`{npm,pnpm,yarn} run test`)
#   3. Built-in default: `node --test` (Node 18+ test runner)
#
# Result-count parsing strategy: tee the runner output to a tempfile,
# detect known runner formats (vitest, jest, node:test, mocha-tap), and
# emit homeboy's TestResults envelope. Unknown formats fall back to
# {total: ?, passed: ?, failed: 0|1 based on exit code, partial: "unknown-runner"}.
# Exit code remains the primary signal — the JSON is decoration.
#
# Standard env vars:
#   HOMEBOY_EXTENSION_PATH       — path to this extension
#   HOMEBOY_COMPONENT_PATH       — path to the Node.js project
#   HOMEBOY_TEST_RESULTS_FILE    — where to write TestResults envelope
#   HOMEBOY_NODE_TEST_COMMAND    — override the test command entirely
#   HOMEBOY_DEBUG                — verbose

if ((BASH_VERSINFO[0] < 4)); then
    echo "ERROR: bash 4.0+ required (found ${BASH_VERSION})" >&2
    exit 1
fi

FAILED_STEP=""
FAILURE_OUTPUT=""

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib/resolve-context.sh
source "${SCRIPT_DIR}/../lib/resolve-context.sh"
homeboy_resolve_context
homeboy_require_package_json
homeboy_detect_package_manager

# Source the homeboy runtime helper if available — provides the canonical
# homeboy_write_test_results function. Path provided by core via
# HOMEBOY_RUNTIME_WRITE_TEST_RESULTS; fall back to inline writer if not set
# (direct invocation outside homeboy).
WRITE_TEST_RESULTS_HELPER="${HOMEBOY_RUNTIME_WRITE_TEST_RESULTS:-}"
if [ -n "$WRITE_TEST_RESULTS_HELPER" ] && [ -f "$WRITE_TEST_RESULTS_HELPER" ]; then
    # shellcheck source=/dev/null
    source "$WRITE_TEST_RESULTS_HELPER"
else
    # Inline fallback that matches the canonical writer's contract.
    homeboy_write_test_results() {
        local total="${1:-0}" passed="${2:-0}" failed="${3:-0}" skipped="${4:-0}" partial="${5:-}"
        if [ -n "${HOMEBOY_TEST_RESULTS_FILE:-}" ]; then
            if [ -n "$partial" ]; then
                cat > "$HOMEBOY_TEST_RESULTS_FILE" <<JSONEOF
{"total":${total},"passed":${passed},"failed":${failed},"skipped":${skipped},"partial":"${partial}"}
JSONEOF
            else
                cat > "$HOMEBOY_TEST_RESULTS_FILE" <<JSONEOF
{"total":${total},"passed":${passed},"failed":${failed},"skipped":${skipped}}
JSONEOF
            fi
        fi
        echo "[test-results] Total: ${total}, Passed: ${passed}, Failed: ${failed}, Skipped: ${skipped}${partial:+ ($partial)}" >&2
    }
fi

print_failure_summary() {
    if [ -n "$FAILED_STEP" ]; then
        echo ""
        echo "============================================"
        echo "TEST FAILED: $FAILED_STEP"
        echo "============================================"
        if [ -n "$FAILURE_OUTPUT" ]; then
            echo ""
            echo "Error details:"
            echo "$FAILURE_OUTPUT"
        fi
    fi
}
trap print_failure_summary EXIT

# Resolve the test command.
if [ -n "${HOMEBOY_NODE_TEST_COMMAND:-}" ]; then
    TEST_CMD="$HOMEBOY_NODE_TEST_COMMAND"
elif homeboy_has_npm_script "test"; then
    TEST_CMD="$PKG_RUN test"
else
    # Node 18+ ships a built-in test runner. This is the right default
    # for tiny projects without an explicit script.
    TEST_CMD="node --test"
fi

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: test command: $TEST_CMD" >&2
fi

echo "Running Node.js tests..."
echo "  Component: ${COMPONENT_ID} (${PROJECT_PATH})"
echo "  Command:   ${TEST_CMD}"
echo ""

cd "$PROJECT_PATH"

OUTPUT_FILE=$(mktemp "${TMPDIR:-/tmp}/homeboy-node-test.XXXXXX")
set +e
# shellcheck disable=SC2086 # word-splitting is intentional here
$TEST_CMD "$@" 2>&1 | tee "$OUTPUT_FILE"
TEST_EXIT=${PIPESTATUS[0]}
set -e

OUTPUT="$(cat "$OUTPUT_FILE")"
rm -f "$OUTPUT_FILE"

# ── Parse result counts by runner detection ──
# Runners we recognize, in priority order. Each match block sets
# total/passed/failed/skipped and PARTIAL_LABEL if recognition is
# heuristic. First match wins.

PARSED=0
TOTAL=0
PASSED=0
FAILED=0
SKIPPED=0
PARTIAL_LABEL=""

# Vitest summary lines look like:
#   Test Files  3 passed (3)
#   Tests       42 passed | 1 skipped (43)
if [ $PARSED -eq 0 ] && echo "$OUTPUT" | grep -qE "^[[:space:]]*Tests[[:space:]]+[0-9]+ (passed|failed|skipped)"; then
    LINE=$(echo "$OUTPUT" | grep -E "^[[:space:]]*Tests[[:space:]]+[0-9]" | tail -1)
    PASSED=$(echo "$LINE" | grep -oE "[0-9]+ passed" | grep -oE "[0-9]+" | head -1 || echo 0)
    FAILED=$(echo "$LINE" | grep -oE "[0-9]+ failed" | grep -oE "[0-9]+" | head -1 || echo 0)
    SKIPPED=$(echo "$LINE" | grep -oE "[0-9]+ skipped" | grep -oE "[0-9]+" | head -1 || echo 0)
    TOTAL=$(echo "$LINE" | grep -oE "\([0-9]+\)" | tr -d '()' | tail -1 || echo 0)
    [ -z "$TOTAL" ] && TOTAL=$((PASSED + FAILED + SKIPPED))
    PARSED=1
fi

# Jest summary lines:
#   Tests:       1 failed, 41 passed, 42 total
if [ $PARSED -eq 0 ] && echo "$OUTPUT" | grep -qE "^Tests:[[:space:]]+.*[0-9]+ total"; then
    LINE=$(echo "$OUTPUT" | grep -E "^Tests:" | tail -1)
    PASSED=$(echo "$LINE" | grep -oE "[0-9]+ passed" | grep -oE "[0-9]+" | head -1 || echo 0)
    FAILED=$(echo "$LINE" | grep -oE "[0-9]+ failed" | grep -oE "[0-9]+" | head -1 || echo 0)
    SKIPPED=$(echo "$LINE" | grep -oE "[0-9]+ skipped" | grep -oE "[0-9]+" | head -1 || echo 0)
    TOTAL=$(echo "$LINE" | grep -oE "[0-9]+ total" | grep -oE "[0-9]+" | head -1 || echo 0)
    PARSED=1
fi

# node --test TAP-ish summary:
#   # tests 42
#   # pass  41
#   # fail  1
#   # skipped 0
if [ $PARSED -eq 0 ] && echo "$OUTPUT" | grep -qE "^# tests[[:space:]]+[0-9]+"; then
    TOTAL=$(echo "$OUTPUT" | grep -oE "^# tests[[:space:]]+[0-9]+" | grep -oE "[0-9]+" | head -1 || echo 0)
    PASSED=$(echo "$OUTPUT" | grep -oE "^# pass[[:space:]]+[0-9]+" | grep -oE "[0-9]+" | head -1 || echo 0)
    FAILED=$(echo "$OUTPUT" | grep -oE "^# fail[[:space:]]+[0-9]+" | grep -oE "[0-9]+" | head -1 || echo 0)
    SKIPPED=$(echo "$OUTPUT" | grep -oE "^# skipped[[:space:]]+[0-9]+" | grep -oE "[0-9]+" | head -1 || echo 0)
    PARSED=1
fi

# Mocha TAP plan + ok counts:
#   1..42
#   ok 1 ...
if [ $PARSED -eq 0 ] && echo "$OUTPUT" | grep -qE "^1\.\.[0-9]+"; then
    TOTAL=$(echo "$OUTPUT" | grep -oE "^1\.\.[0-9]+" | head -1 | sed 's/^1\.\.//' || echo 0)
    PASSED=$(echo "$OUTPUT" | grep -cE "^ok [0-9]" || true)
    FAILED=$(echo "$OUTPUT" | grep -cE "^not ok [0-9]" || true)
    SKIPPED=$(echo "$OUTPUT" | grep -cE "# (SKIP|skip)" || true)
    PARSED=1
fi

# Unknown runner — fall back to exit-code semantics.
if [ $PARSED -eq 0 ]; then
    if [ $TEST_EXIT -eq 0 ]; then
        TOTAL=0; PASSED=0; FAILED=0; SKIPPED=0
    else
        TOTAL=1; PASSED=0; FAILED=1; SKIPPED=0
    fi
    PARTIAL_LABEL="unknown-runner"
fi

homeboy_write_test_results "$TOTAL" "$PASSED" "$FAILED" "$SKIPPED" "$PARTIAL_LABEL"

if [ $TEST_EXIT -ne 0 ]; then
    FAILED_STEP="Tests failed (exit $TEST_EXIT)"
    # Show the last 20 lines as failure context — full output already streamed above.
    FAILURE_OUTPUT="$(echo "$OUTPUT" | tail -20)"
fi

exit $TEST_EXIT
