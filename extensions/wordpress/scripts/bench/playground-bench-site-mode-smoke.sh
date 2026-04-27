#!/usr/bin/env bash
#
# Installed-site + Playground blueprint bench smoke test.
#
# Runs the bench-site-mode fixture twice against the same shared-state
# directory. The first run prepares a persisted /wordpress tree and applies a
# blueprint. The second run reuses that installed site and asserts the bench
# runner skips wp-phpunit install.php in favour of wp-load.php.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
FIXTURE_DIR="${EXTENSION_PATH}/tests/fixtures/bench-site-mode"

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

if ! command -v jq >/dev/null 2>&1; then
    echo "ERROR: jq required for JSON assertions in this smoke." >&2
    exit 1
fi

RESULTS_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/bench-site-mode-smoke.XXXXXX")
SHARED_STATE_DIR=$(mktemp -d "${TMPDIR:-/tmp}/bench-site-mode-smoke.XXXXXX")

cleanup() {
    rm -f "$RESULTS_TMPFILE"
    rm -rf "$SHARED_STATE_DIR"
}
trap cleanup EXIT

SETTINGS_JSON=$(cat <<'JSON'
{
  "bench_site_mode": "installed",
  "playground_blueprint": {
    "steps": [
      {
        "step": "setSiteOptions",
        "options": {
          "blogname": "Blueprint Bench Smoke"
        }
      }
    ]
  }
}
JSON
)

run_fixture() {
    HOMEBOY_BENCH_RESULTS_FILE="$RESULTS_TMPFILE" \
    HOMEBOY_BENCH_ITERATIONS=1 \
    HOMEBOY_BENCH_SHARED_STATE="$SHARED_STATE_DIR" \
    HOMEBOY_BENCH_INSTANCE_ID=0 \
    HOMEBOY_BENCH_CONCURRENCY=1 \
    HOMEBOY_COMPONENT_ID=bench-site-mode \
    HOMEBOY_COMPONENT_PATH="$FIXTURE_DIR" \
    HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_SETTINGS_JSON="$SETTINGS_JSON" \
        bash "${SCRIPT_DIR}/bench-runner.sh"
}

echo "============================================"
echo "Playground bench installed-site smoke test"
echo "============================================"
echo "Fixture:       $FIXTURE_DIR"
echo "Shared state:  $SHARED_STATE_DIR"
echo ""

run_fixture
run_fixture

if [ ! -s "$RESULTS_TMPFILE" ]; then
    echo "ERROR: results file empty or missing at $RESULTS_TMPFILE" >&2
    exit 1
fi

echo ""
echo "--- Results envelope ---"
cat "$RESULTS_TMPFILE"
echo ""

bootstrap_keys=$(jq -r '.scenarios[0].metrics | keys | join(",")' "$RESULTS_TMPFILE")
if [[ "$bootstrap_keys" != *"load_wordpress_ms"* ]]; then
    echo "ERROR: expected load_wordpress_ms in bootstrap metrics, got: $bootstrap_keys" >&2
    exit 1
fi
if [[ "$bootstrap_keys" == *"install_ms"* ]]; then
    echo "ERROR: installed-site mode should not emit install_ms, got: $bootstrap_keys" >&2
    exit 1
fi

READ_BACK_LOG="${SHARED_STATE_DIR}/site-mode-read-back.log"
if [ ! -f "$READ_BACK_LOG" ]; then
    echo "ERROR: workload did not write site-mode-read-back.log" >&2
    exit 1
fi

echo ""
echo "--- Read-back log ---"
cat "$READ_BACK_LOG"
echo ""

if ! grep -q 'site_title=Blueprint Bench Smoke' "$READ_BACK_LOG"; then
    echo "ERROR: blueprint site option did not reach workload" >&2
    exit 1
fi

if [ ! -f "${SHARED_STATE_DIR}/wordpress/wp-load.php" ]; then
    echo "ERROR: installed-site mode did not persist /wordpress into shared state" >&2
    exit 1
fi

echo "============================================"
echo "✓ installed-site + blueprint smoke test PASSED"
echo "============================================"
