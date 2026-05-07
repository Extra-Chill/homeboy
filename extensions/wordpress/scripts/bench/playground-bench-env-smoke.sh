#!/usr/bin/env bash
#
# bench_env passthrough smoke test.
#
# Runs the fixture at wordpress/tests/fixtures/bench-env/ through the
# bench dispatcher with a synthetic HOMEBOY_SETTINGS_JSON containing
# bench_env, and asserts:
#   - The declared env vars are readable via getenv() inside Playground.
#   - $_ENV contains them too (for code that reads $_ENV directly).
#   - Values arrive as strings (PHP env-var convention).
#
# Manual integration test, not part of CI. Run after changes to:
#   - playground-bench-runner.php (the putenv() loop)
#   - bench-runner-playground.sh (the BENCH_ENV_JSON extraction + sed)
#   - playground-runner.php (test-runner mirror)
#   - test-runner-playground.sh (test-runner mirror)
#
# Usage: bash wordpress/scripts/bench/playground-bench-env-smoke.sh
# Exit:  0 = round-trips, non-zero = regression

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
FIXTURE_DIR="${EXTENSION_PATH}/tests/fixtures/bench-env"

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

RESULTS_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/bench-env-smoke.XXXXXX")
SHARED_STATE_DIR=$(mktemp -d "${TMPDIR:-/tmp}/bench-env-smoke.XXXXXX")

cleanup() {
    rm -f "$RESULTS_TMPFILE"
    rm -rf "$SHARED_STATE_DIR"
}
trap cleanup EXIT

ITERATIONS=2

# Synthesize the merged settings JSON the dispatcher would receive from
# homeboy core. Real components declare this under
# extensions.wordpress.settings.bench_env in their homeboy.json.
#
# BENCH_ENV_FIXTURE_METAS is the regression value for the sed-replacement
# escape bug: it contains `\` (via the JSON-escaped `\"` sequences) and a
# literal `&`, both of which GNU sed mangles in `s` replacement strings
# unless the runner escapes them before substituting BENCH_ENV_JSON into
# the PHP template. Without the fix, json_decode() of BENCH_ENV_JSON
# returns null and ALL bench_env keys silently drop — including unrelated
# bystanders like BENCH_ENV_FIXTURE_STR.
SETTINGS_JSON=$(cat <<'JSON'
{
  "bench_env": {
    "BENCH_ENV_FIXTURE_STR": "hello",
    "BENCH_ENV_FIXTURE_NUM": "42",
    "BENCH_ENV_FIXTURE_METAS": "{\"text\":\"a & b\",\"path\":\"C:\\\\tmp\"}"
  }
}
JSON
)

echo "============================================"
echo "Playground bench bench_env smoke test"
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
HOMEBOY_COMPONENT_ID=bench-env \
HOMEBOY_COMPONENT_PATH="$FIXTURE_DIR" \
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_SETTINGS_JSON="$SETTINGS_JSON" \
    bash "${SCRIPT_DIR}/bench-runner.sh"

if [ ! -s "$RESULTS_TMPFILE" ]; then
    echo "ERROR: results file empty or missing at $RESULTS_TMPFILE" >&2
    exit 1
fi

READ_BACK_LOG="${SHARED_STATE_DIR}/env-read-back.log"
if [ ! -f "$READ_BACK_LOG" ]; then
    echo "ERROR: workload did not write env-read-back.log" >&2
    exit 1
fi

echo ""
echo "--- Read-back log ---"
cat "$READ_BACK_LOG"
echo ""

# Assert each declared env var is readable via getenv().
if ! grep -q "\"BENCH_ENV_FIXTURE_STR_getenv\":\"'hello'\"" "$READ_BACK_LOG"; then
    echo "ERROR: BENCH_ENV_FIXTURE_STR did not round-trip via getenv()" >&2
    exit 1
fi
if ! grep -q "\"BENCH_ENV_FIXTURE_NUM_getenv\":\"'42'\"" "$READ_BACK_LOG"; then
    echo "ERROR: BENCH_ENV_FIXTURE_NUM did not round-trip via getenv()" >&2
    exit 1
fi
if ! grep -q "\"BENCH_ENV_FIXTURE_STR_in_env\":\"yes\"" "$READ_BACK_LOG"; then
    echo "ERROR: BENCH_ENV_FIXTURE_STR not in \$_ENV" >&2
    exit 1
fi
if ! grep -q "\"BENCH_ENV_FIXTURE_STR_env_value\":\"hello\"" "$READ_BACK_LOG"; then
    echo "ERROR: \$_ENV['BENCH_ENV_FIXTURE_STR'] wrong value" >&2
    exit 1
fi

# BENCH_ENV_FIXTURE_METAS carries the sed-replacement-escape regression
# payload (see settings JSON above). The workload var_export()s the
# getenv() result, so we expect:
#   "BENCH_ENV_FIXTURE_METAS_getenv":"'{\"text\":\"a & b\",\"path\":\"C:\\\\tmp\"}'"
# which after JSON-encoding (json_encode in the workload) and grep-quoting
# becomes a long literal — keep the assertion small and split into the
# two corruption signatures: the literal `&` survives and a `\"` survives.
if ! grep -q '"BENCH_ENV_FIXTURE_METAS_getenv":"' "$READ_BACK_LOG"; then
    echo "ERROR: BENCH_ENV_FIXTURE_METAS_getenv missing from read-back log" >&2
    exit 1
fi
if ! grep -q 'a & b' "$READ_BACK_LOG"; then
    echo "ERROR: literal '&' did not round-trip in BENCH_ENV_FIXTURE_METAS." >&2
    echo "       Likely cause: BENCH_ENV_JSON sed substitution treated '&' as a" >&2
    echo "       backreference and corrupted the JSON before json_decode()." >&2
    exit 1
fi
if ! grep -q '\\"text\\"' "$READ_BACK_LOG"; then
    echo "ERROR: '\\\"' did not round-trip in BENCH_ENV_FIXTURE_METAS." >&2
    echo "       Likely cause: BENCH_ENV_JSON sed substitution dropped backslashes" >&2
    echo "       in the replacement string, corrupting the JSON before json_decode()." >&2
    exit 1
fi

echo "============================================"
echo "✓ bench_env smoke test PASSED"
echo "  All declared env vars round-trip through getenv() and \$_ENV,"
echo "  including values containing '\\\\' and '&'."
echo "============================================"
