#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
TEST_RUNNER="${SCRIPT_DIR}/test-runner-playground.sh"
BENCH_RUNNER="${SCRIPT_DIR}/../bench/bench-runner-playground.sh"
HOST_FIXTURE_DIR="${EXTENSION_PATH}/tests/fixtures/playground-init-lifecycle-host"
RESULTS_TMPFILE="$(mktemp "${TMPDIR:-/tmp}/playground-init-lifecycle.XXXXXX.json")"

cleanup() {
    rm -f "$RESULTS_TMPFILE"
}
trap cleanup EXIT

if [ ! -f "${EXTENSION_PATH}/node_modules/.bin/wp-playground-cli" ]; then
    echo "ERROR: @wp-playground/cli not installed." >&2
    echo "Run: cd ${EXTENSION_PATH} && npm install" >&2
    exit 1
fi

if [ ! -d "${EXTENSION_PATH}/vendor/wp-phpunit" ]; then
    echo "ERROR: wp-phpunit not installed." >&2
    echo "Run: cd ${EXTENSION_PATH} && composer install" >&2
    exit 1
fi

if ! command -v jq >/dev/null 2>&1; then
    echo "ERROR: jq required for JSON assertions in this smoke." >&2
    exit 1
fi

SETTINGS_JSON='{"playground_wordpress_version":"7.0"}'

assert_no_connector_notice() {
    local output="$1"
    local label="$2"

    if grep -Fq 'WP_Connector_Registry::set_instance' <<< "$output"; then
        echo "ERROR: ${label} emitted the WP 7.0 connector registry timing notice" >&2
        echo "$output" >&2
        exit 1
    fi

    if grep -Fqi 'connector registry instance must be set during' <<< "$output"; then
        echo "ERROR: ${label} emitted the WP 7.0 connector registry timing notice" >&2
        echo "$output" >&2
        exit 1
    fi
}

echo "============================================"
echo "Playground init lifecycle smoke (#443)"
echo "============================================"
echo "Host fixture: $HOST_FIXTURE_DIR"
echo "WordPress:    7.0"
echo ""

set +e
test_output=$(HOMEBOY_COMPONENT_ID=playground-init-lifecycle-host \
    HOMEBOY_COMPONENT_PATH="$HOST_FIXTURE_DIR" \
    HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_SETTINGS_JSON="$SETTINGS_JSON" \
    bash "$TEST_RUNNER" 2>&1)
test_exit=$?
set -e

if [ $test_exit -ne 0 ]; then
    echo "ERROR: Playground test runner failed" >&2
    echo "$test_output" >&2
    exit $test_exit
fi
assert_no_connector_notice "$test_output" "test runner"
echo "✓ test runner replayed deferred init callbacks in init context"

set +e
bench_output=$(HOMEBOY_BENCH_RESULTS_FILE="$RESULTS_TMPFILE" \
    HOMEBOY_BENCH_ITERATIONS=1 \
    HOMEBOY_COMPONENT_ID=playground-init-lifecycle-host \
    HOMEBOY_COMPONENT_PATH="$HOST_FIXTURE_DIR" \
    HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_SETTINGS_JSON="$SETTINGS_JSON" \
    bash "$BENCH_RUNNER" 2>&1)
bench_exit=$?
set -e

if [ $bench_exit -ne 0 ]; then
    echo "ERROR: Playground bench runner failed" >&2
    echo "$bench_output" >&2
    exit $bench_exit
fi
assert_no_connector_notice "$bench_output" "bench runner"

metric=$(jq -r '.scenarios[] | select(.id == "assert-init-lifecycle") | .metrics.deferred_init_context_ok_mean // "missing"' "$RESULTS_TMPFILE")
if [ "$metric" != "1" ]; then
    echo "ERROR: bench workload did not verify deferred init lifecycle context (got $metric)" >&2
    cat "$RESULTS_TMPFILE" >&2
    exit 1
fi
echo "✓ bench runner replayed deferred init callbacks in init context"

echo ""
echo "============================================"
echo "✓ Playground init lifecycle smoke PASSED"
echo "============================================"
