#!/usr/bin/env bash
#
# Validation-dependency db.php skip smoke.
#
# Asserts dependency plugin discovery ignores db.php even when that drop-in has
# a Plugin Name header and sorts before the dependency's actual entry file.
#
# Usage: bash wordpress/scripts/bench/playground-bench-dep-dropin-skip-smoke.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
HOST_FIXTURE_DIR="${EXTENSION_PATH}/tests/fixtures/bench-plugins-loaded-host"

if [ ! -d "$HOST_FIXTURE_DIR" ]; then
    echo "ERROR: host fixture not found at $HOST_FIXTURE_DIR" >&2
    exit 1
fi
if ! command -v jq >/dev/null 2>&1; then
    echo "ERROR: jq required for JSON assertions in this smoke." >&2
    exit 1
fi

TMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/homeboy-dep-dropin.XXXXXX")
RESULTS_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/bench-dep-dropin-skip.XXXXXX")
cleanup() {
    rm -rf "$TMP_DIR"
    rm -f "$RESULTS_TMPFILE"
}
trap cleanup EXIT

cat > "$TMP_DIR/db.php" <<'PHP'
<?php
/**
 * Plugin Name: Dependency Drop-in That Must Be Skipped
 */

throw new RuntimeException( 'Dependency db.php was loaded as a plugin entry.' );
PHP

cat > "$TMP_DIR/plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: Dependency Actual Plugin Entry
 */

add_action( 'plugins_loaded', static function (): void {
    $GLOBALS['homeboy_bench_426_dep_plugins_loaded_fired'] = true;
} );
PHP

SETTINGS_JSON=$(jq -n --arg dep "$TMP_DIR" '{"validation_dependencies": [$dep]}')

HOMEBOY_BENCH_RESULTS_FILE="$RESULTS_TMPFILE" \
HOMEBOY_BENCH_ITERATIONS=1 \
HOMEBOY_BENCH_WARMUP_ITERATIONS=0 \
HOMEBOY_COMPONENT_ID=bench-plugins-loaded-host \
HOMEBOY_COMPONENT_PATH="$HOST_FIXTURE_DIR" \
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_SETTINGS_JSON="$SETTINGS_JSON" \
    bash "${SCRIPT_DIR}/bench-runner.sh"

scenario='.scenarios[] | select(.id == "assert-dep-plugins-loaded")'
fired=$(jq -r "$scenario | .metrics.dep_plugins_loaded_fired_mean // \"missing\"" "$RESULTS_TMPFILE")
if [ "$fired" != "1" ]; then
    echo "ERROR: dep plugin entry was not loaded after skipping db.php (got $fired)" >&2
    exit 1
fi

echo "✓ Dependency db.php skip smoke PASSED"
