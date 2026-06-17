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
#   HOMEBOY_TEST_FAILURES_FILE   — where to write parsed failure details
#   HOMEBOY_NODE_TEST_COMMAND    — override the test command entirely
#   HOMEBOY_NODE_TARGETED_TEST_SCRIPT — npm script to use when args are present
#   HOMEBOY_CHANGED_TEST_FILES   — newline-separated test files selected by core
#   HOMEBOY_DEBUG                — verbose

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER_PRELUDE="${HOMEBOY_RUNTIME_RUNNER_PRELUDE:?HOMEBOY_RUNTIME_RUNNER_PRELUDE is required}"
COMMAND_CAPTURE_HELPER="${HOMEBOY_RUNTIME_COMMAND_CAPTURE:?HOMEBOY_RUNTIME_COMMAND_CAPTURE is required}"
SETTINGS_HELPER="${HOMEBOY_RUNTIME_SETTINGS_HELPER:-${SCRIPT_DIR}/../lib/settings.sh}"
# shellcheck source=/dev/null
source "$RUNNER_PRELUDE"
homeboy_runner_init --bash 4 --sidecar-writer --failure-trap
# shellcheck source=/dev/null
source "$SETTINGS_HELPER"
# shellcheck source=/dev/null
source "$COMMAND_CAPTURE_HELPER"
# shellcheck source=../lib/node-helpers.sh
source "${SCRIPT_DIR}/../lib/node-helpers.sh"
homeboy_require_package_json
homeboy_detect_package_manager
homeboy_ensure_node_dependencies

RUNNER_ARGS=("$@")
if [ -n "${HOMEBOY_TEST_RUNNER_ARGS:-}" ]; then
    while IFS= read -r selected_test_arg; do
        [ -n "$selected_test_arg" ] || continue
        RUNNER_ARGS+=("$selected_test_arg")
    done <<< "$HOMEBOY_TEST_RUNNER_ARGS"
elif [ -n "${HOMEBOY_CHANGED_TEST_FILES:-}" ]; then
    while IFS= read -r selected_test_file; do
        [ -n "$selected_test_file" ] || continue
        RUNNER_ARGS+=("$selected_test_file")
    done <<< "$HOMEBOY_CHANGED_TEST_FILES"
fi

homeboy_node_package_value() {
    local expression="$1"
    PACKAGE_JSON_PATH="${PROJECT_PATH}/package.json" \
    node -e "
        const pkg = require(process.env.PACKAGE_JSON_PATH);
        const value = (${expression});
        if (typeof value === 'string' && value) console.log(value);
    " 2>/dev/null || true
}

homeboy_node_script_command() {
    local script_name="$1"
    printf '%s --' "$(homeboy_project_run_script_command "$script_name")"
}

homeboy_node_targeted_test_script() {
    local configured_script="${HOMEBOY_NODE_TARGETED_TEST_SCRIPT:-}"
    if [ -z "$configured_script" ]; then
        configured_script="$(homeboy_setting node_targeted_test_script '.node_targeted_test_script // .targeted_test_script // .test_script // .testing.targeted_test_script // empty')"
    fi

    if [ -n "$configured_script" ]; then
        if ! homeboy_has_npm_script "$configured_script"; then
            echo "Error: targeted Node.js test script '${configured_script}' is not defined in package.json" >&2
            exit 1
        fi
        printf '%s' "$configured_script"
        return 0
    fi

    local package_name
    package_name="$(homeboy_node_package_value "pkg.name || ''")"
    if [ "$package_name" = "gutenberg" ] && homeboy_has_npm_script "test:unit"; then
        printf '%s' "test:unit"
        return 0
    fi

    return 1
}

WRITE_TEST_RESULTS_HELPER="${HOMEBOY_RUNTIME_WRITE_TEST_RESULTS:-}"
if [ -n "$WRITE_TEST_RESULTS_HELPER" ] && [ -f "$WRITE_TEST_RESULTS_HELPER" ]; then
    # shellcheck source=/dev/null
    source "$WRITE_TEST_RESULTS_HELPER"
fi

# Resolve the test command.
if [ -n "${HOMEBOY_NODE_TEST_COMMAND:-}" ]; then
    TEST_CMD="$HOMEBOY_NODE_TEST_COMMAND"
elif [ ${#RUNNER_ARGS[@]} -gt 0 ] && TARGETED_TEST_SCRIPT="$(homeboy_node_targeted_test_script)"; then
    TEST_CMD="$(homeboy_node_script_command "$TARGETED_TEST_SCRIPT")"
elif homeboy_has_npm_script "test"; then
    TEST_CMD="$(homeboy_project_run_script_command test)"
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
if [ -n "${HOMEBOY_CHANGED_TEST_FILES:-}" ]; then
    echo "  Scope:     ${#RUNNER_ARGS[@]} selected test file(s)"
fi
echo ""

cd "$PROJECT_PATH"

homeboy_run_step_capture OUTPUT_FILE TEST_EXIT "Tests failed" -- bash -c "$TEST_CMD \"\$@\"" _ "${RUNNER_ARGS[@]}" || true

OUTPUT="$(cat "$OUTPUT_FILE")"

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
FAILED_TEST_NAME=""
FAILED_TEST_FILE=""
FAILED_ERROR_TYPE=""
FAILED_MESSAGE=""

extract_failed_nx_task() {
    echo "$OUTPUT" | awk '
        /^Failed tasks:[[:space:]]*$/ { in_failed_tasks = 1; next }
        in_failed_tasks && /^[[:space:]]*-[[:space:]]+/ {
            sub(/^[[:space:]]*-[[:space:]]+/, "")
            print
            exit
        }
        in_failed_tasks && NF == 0 { next }
        in_failed_tasks { exit }
    '
}

extract_vitest_failure_line() {
    echo "$OUTPUT" | grep -E "^[[:space:]]*FAIL[[:space:]]+.*[[:space:]]>[[:space:]]" | head -1 || true
}

write_node_failure_json() {
    [ -n "${HOMEBOY_TEST_FAILURES_FILE:-}" ] || return 0
    [ -n "$FAILED_TEST_NAME" ] || return 0
    if ! type homeboy_sidecar_emit >/dev/null 2>&1; then
        echo "Error: HOMEBOY_RUNTIME_SIDECAR_WRITER is required to write test failures" >&2
        return 1
    fi

    FAILURE_JSON="$(HOMEBOY_NODE_TEST_OUTPUT="$OUTPUT" node - "$FAILED_TEST_NAME" "$FAILED_TEST_FILE" "$FAILED_ERROR_TYPE" "$FAILED_MESSAGE" <<'JS'
const crypto = require('node:crypto');

const [testName, testFile, errorType, message] = process.argv.slice(2);
const stdout = process.env.HOMEBOY_NODE_TEST_OUTPUT || '';
const fingerprintInput = [testName, testFile, errorType, message].join('\0');
const fingerprint = crypto.createHash('sha256').update(fingerprintInput).digest('hex');
const stdoutExcerpt = stdout.split(/\r?\n/).slice(-40).join('\n');

console.log(JSON.stringify({
  test_name: testName,
  test_file: testFile,
  error_type: errorType,
  test_id: testName,
  suite: '',
  file: testFile,
  line: 0,
  message,
  failure_type: errorType,
  fingerprint,
  stdout_excerpt: stdoutExcerpt,
  stderr_excerpt: '',
  source_file: testFile,
  source_line: 0
}));
JS
)"
    homeboy_sidecar_emit test.failure "$FAILURE_JSON"
}

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

# Nx wraps the underlying runner output and prints the failed target separately:
#   FAIL src/test/version-detect.spec.ts > Suite > test name
#   Error: Test timed out in 30000ms.
#   Failed tasks:
#   - playground-wordpress:test:vite
# When Vitest times out before writing its final summary, preserve the test
# failure instead of falling back to an opaque `unknown-runner` infrastructure
# failure.
if [ $PARSED -eq 0 ] && echo "$OUTPUT" | grep -qE "^[[:space:]]*FAIL[[:space:]]+.*[[:space:]]>[[:space:]]" && echo "$OUTPUT" | grep -q "Test timed out in"; then
    VITEST_FAILURE_LINE="$(extract_vitest_failure_line)"
    NX_FAILED_TASK="$(extract_failed_nx_task)"

    FAILED_TEST_FILE="$(echo "$VITEST_FAILURE_LINE" | sed -E 's/^[[:space:]]*FAIL[[:space:]]+([^[:space:]]+).*$/\1/')"
    FAILED_TEST_NAME="$(echo "$VITEST_FAILURE_LINE" | sed -E 's/^[[:space:]]*FAIL[[:space:]]+[^>]+>[[:space:]]*//')"
    FAILED_ERROR_TYPE="vitest_timeout"
    FAILED_MESSAGE="$(echo "$OUTPUT" | grep -E "Test timed out in [0-9]+ms" | head -1 | sed -E 's/^[[:space:]]*Error:[[:space:]]*//')"

    if [ -n "$NX_FAILED_TASK" ]; then
        FAILED_MESSAGE="${FAILED_MESSAGE} (Nx task: ${NX_FAILED_TASK})"
    fi

    TOTAL=1
    PASSED=0
    FAILED=1
    SKIPPED=0
    PARTIAL_LABEL="vitest-timeout"
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

if type homeboy_write_test_results >/dev/null 2>&1; then
    homeboy_write_test_results "$TOTAL" "$PASSED" "$FAILED" "$SKIPPED" "$PARTIAL_LABEL"
fi

write_node_failure_json

if [ $TEST_EXIT -ne 0 ]; then
    FAILED_STEP="Tests failed (exit $TEST_EXIT)"
fi

homeboy_cleanup_step_capture "$OUTPUT_FILE"
exit $TEST_EXIT
