#!/usr/bin/env bash
#
# wp_config_defines bench harness end-to-end smoke test.
#
# Runs the fixture at wordpress/tests/fixtures/wp-config-defines/ through
# the bench dispatcher with a synthetic HOMEBOY_SETTINGS_JSON containing
# wp_config_defines, and asserts:
#   - Each declared constant is defined in the Playground PHP runtime.
#   - PHP type round-trips: string stays string, int stays int, true stays bool.
#   - The defines land in wp-tests-config.php (not in the runner template
#     where they'd evaporate before WordPress boot).
#
# Manual integration test, not part of CI. Run after changes to:
#   - playground-bootstrap.php::pg_run_boot_stage() (extra_defines handling)
#   - bench-runner-playground.sh / test-runner-playground.sh (settings
#     extraction + sed substitution)
#   - playground-bench-runner.php / playground-runner.php (placeholder
#     decoding + boot-stage call)
#
# Usage: bash wordpress/scripts/bench/playground-bench-wp-config-defines-smoke.sh
# Exit:  0 = round-trips with type preservation, non-zero = regression

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
FIXTURE_DIR="${EXTENSION_PATH}/tests/fixtures/wp-config-defines"

if [ ! -d "$FIXTURE_DIR" ]; then
    echo "ERROR: fixture not found at $FIXTURE_DIR" >&2
    exit 1
fi

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

RESULTS_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/bench-defines-smoke.XXXXXX")
SHARED_STATE_DIR=$(mktemp -d "${TMPDIR:-/tmp}/bench-defines-smoke.XXXXXX")

cleanup() {
    rm -f "$RESULTS_TMPFILE"
    rm -rf "$SHARED_STATE_DIR"
}
trap cleanup EXIT

ITERATIONS=2

# Synthesize the merged settings JSON the dispatcher would normally
# receive from homeboy core. Real components declare this under
# extensions.wordpress.settings.wp_config_defines in their homeboy.json.
SETTINGS_JSON=$(cat <<'JSON'
{
  "wp_config_defines": {
    "WP_CONFIG_FIXTURE_STRING": "hello",
    "WP_CONFIG_FIXTURE_INT": 42,
    "WP_CONFIG_FIXTURE_BOOL": true
  }
}
JSON
)

echo "============================================"
echo "Playground bench wp_config_defines smoke test"
echo "============================================"
echo "Fixture:       $FIXTURE_DIR"
echo "Shared state:  $SHARED_STATE_DIR"
echo "Settings:      $SETTINGS_JSON"
echo ""

HOMEBOY_BENCH_RESULTS_FILE="$RESULTS_TMPFILE" \
HOMEBOY_BENCH_ITERATIONS="$ITERATIONS" \
HOMEBOY_BENCH_SHARED_STATE="$SHARED_STATE_DIR" \
HOMEBOY_BENCH_INSTANCE_ID=0 \
HOMEBOY_BENCH_CONCURRENCY=1 \
HOMEBOY_COMPONENT_ID=wp-config-defines \
HOMEBOY_COMPONENT_PATH="$FIXTURE_DIR" \
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_SETTINGS_JSON="$SETTINGS_JSON" \
    bash "${SCRIPT_DIR}/bench-runner.sh"

if [ ! -s "$RESULTS_TMPFILE" ]; then
    echo "ERROR: results file empty or missing at $RESULTS_TMPFILE" >&2
    exit 1
fi

echo ""
echo "--- Results envelope ---"
cat "$RESULTS_TMPFILE"
echo ""

READ_BACK_LOG="${SHARED_STATE_DIR}/defines-read-back.log"
if [ ! -f "$READ_BACK_LOG" ]; then
    echo "ERROR: workload did not write defines-read-back.log" >&2
    echo "       (constants may not be reaching the workload)" >&2
    exit 1
fi

echo ""
echo "--- Read-back log ---"
cat "$READ_BACK_LOG"
echo ""

# Assert each declared constant round-trips with the right value.
if ! grep -q "string='hello'" "$READ_BACK_LOG"; then
    echo "ERROR: WP_CONFIG_FIXTURE_STRING did not round-trip with value 'hello'" >&2
    exit 1
fi
if ! grep -q 'int=42' "$READ_BACK_LOG"; then
    echo "ERROR: WP_CONFIG_FIXTURE_INT did not round-trip with value 42" >&2
    exit 1
fi
if ! grep -q 'bool=true' "$READ_BACK_LOG"; then
    echo "ERROR: WP_CONFIG_FIXTURE_BOOL did not round-trip with value true" >&2
    exit 1
fi

# Type preservation — the whole point of var_export().
if ! grep -q 'string_type=string' "$READ_BACK_LOG"; then
    echo "ERROR: WP_CONFIG_FIXTURE_STRING wrong runtime type" >&2
    exit 1
fi
if ! grep -q 'int_type=integer' "$READ_BACK_LOG"; then
    echo "ERROR: WP_CONFIG_FIXTURE_INT wrong runtime type (got string instead of integer?)" >&2
    exit 1
fi
if ! grep -q 'bool_type=boolean' "$READ_BACK_LOG"; then
    echo "ERROR: WP_CONFIG_FIXTURE_BOOL wrong runtime type (got string instead of boolean?)" >&2
    exit 1
fi

echo "============================================"
echo "✓ wp_config_defines smoke test PASSED"
echo "  All three constants round-trip with type preservation."
echo "============================================"
