#!/usr/bin/env bash
set -euo pipefail

# WP Codebox raw extra_plugins Composer autoload smoke (homeboy-extensions#1398).
#
# This intentionally does not run Composer in Homeboy Extensions. It sends a raw
# plugin source with composer.json and no vendor/autoload.php through the normal
# WordPress/WP Codebox recipe flow, then asserts WP Codebox prepared the plugin
# before activation/workload execution.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
BENCH_RUNNER="${EXTENSION_PATH}/scripts/bench/bench-runner-wp-codebox.sh"
TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wp-codebox-extra-plugins-autoload.XXXXXX")"
cleanup() {
    local status="$?"
    if [ "$status" -eq 0 ]; then
        rm -rf "$TMP_ROOT"
    else
        echo "Preserving smoke workspace for diagnostics: $TMP_ROOT" >&2
    fi
}
trap cleanup EXIT

COMPONENT_ROOT="${TMP_ROOT}/host-component"
EXTRA_PLUGIN_ROOT="${TMP_ROOT}/hbex-extra-plugin-autoload"
ARTIFACTS_DIR="${TMP_ROOT}/artifacts"
RESULTS_FILE="${TMP_ROOT}/bench-results.json"
RESOLVE_HELPER="${TMP_ROOT}/resolve-context.sh"
BENCH_HELPER="${TMP_ROOT}/bench-helper.sh"

mkdir -p "${COMPONENT_ROOT}/tests/bench" "${EXTRA_PLUGIN_ROOT}/src" "$ARTIFACTS_DIR"

cat > "${COMPONENT_ROOT}/host-component.php" <<'PHP'
<?php
/**
 * Plugin Name: HBEX WP Codebox Extra Plugins Autoload Host
 */
PHP

cat > "${COMPONENT_ROOT}/tests/bench/assert-extra-plugin-autoload.php" <<'PHP'
<?php
return static function (): array {
    if ( ! class_exists( 'HbexExtraPluginsAutoload\\Canary' ) ) {
        throw new RuntimeException( 'Extra plugin Composer-autoloaded class is unavailable.' );
    }

    if ( 'loaded' !== get_option( 'hbex_extra_plugins_autoload_canary' ) ) {
        throw new RuntimeException( 'Extra plugin activation/runtime marker was not written.' );
    }

    return array(
        'metrics' => array(
            'autoloaded_extra_plugin' => 1,
        ),
    );
};
PHP

cat > "${EXTRA_PLUGIN_ROOT}/composer.json" <<'JSON'
{
  "autoload": {
    "psr-4": {
      "HbexExtraPluginsAutoload\\": "src/"
    }
  }
}
JSON

cat > "${EXTRA_PLUGIN_ROOT}/src/Canary.php" <<'PHP'
<?php
namespace HbexExtraPluginsAutoload;

final class Canary {
    public static function mark(): void {
        update_option( 'hbex_extra_plugins_autoload_canary', 'loaded', false );
    }
}
PHP

cat > "${EXTRA_PLUGIN_ROOT}/hbex-extra-plugin-autoload.php" <<'PHP'
<?php
/**
 * Plugin Name: HBEX WP Codebox Extra Plugin Autoload Canary
 */

require_once __DIR__ . '/vendor/autoload.php';

add_action( 'plugins_loaded', static function (): void {
    \HbexExtraPluginsAutoload\Canary::mark();
} );
PHP

if [ -e "${EXTRA_PLUGIN_ROOT}/vendor/autoload.php" ]; then
    echo "ERROR: smoke fixture unexpectedly has a pre-existing Composer autoload file" >&2
    exit 1
fi

cat > "$RESOLVE_HELPER" <<'SH'
homeboy_resolve_context() {
    PLUGIN_PATH="$HOMEBOY_COMPONENT_PATH"
    COMPONENT_ID="$HOMEBOY_COMPONENT_ID"
}
SH

cat > "$BENCH_HELPER" <<'SH'
homeboy_write_empty_bench_results() {
    local component="$1"
    local iterations="$2"
    local results_file="$3"
    printf '{"schema":"homeboy/bench-results/v1","component_id":"%s","iterations":%s,"scenarios":[]}\n' "$component" "$iterations" > "$results_file"
}
SH

SETTINGS_JSON=$(jq -nc \
    --arg extraSource "$EXTRA_PLUGIN_ROOT" \
    '{
        wp_codebox_extra_plugins: [{
            source: $extraSource,
            slug: "hbex-extra-plugin-autoload",
            pluginFile: "hbex-extra-plugin-autoload/hbex-extra-plugin-autoload.php",
            activate: true
        }]
    }')

echo "============================================"
echo "WP Codebox raw extra_plugins Composer autoload smoke (#1398)"
echo "============================================"
echo "Host fixture:  $COMPONENT_ROOT"
echo "Extra plugin:  $EXTRA_PLUGIN_ROOT"
echo "Artifacts:     $ARTIFACTS_DIR"
echo ""

set +e
output=$(HOMEBOY_COMPONENT_ID=host-component \
    HOMEBOY_COMPONENT_PATH="$COMPONENT_ROOT" \
    HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_HELPER" \
    HOMEBOY_RUNTIME_BENCH_HELPER_SH="$BENCH_HELPER" \
    HOMEBOY_RUNTIME_FAILURE_TRAP="" \
    HOMEBOY_SETTINGS_JSON="$SETTINGS_JSON" \
    HOMEBOY_BENCH_RESULTS_FILE="$RESULTS_FILE" \
    HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR="$ARTIFACTS_DIR" \
    HOMEBOY_BENCH_ITERATIONS=1 \
    HOMEBOY_BENCH_WARMUP_ITERATIONS=0 \
    bash "$BENCH_RUNNER" 2>&1)
status=$?
set -e

printf '%s\n' "$output"

if [ "$status" -ne 0 ]; then
    echo "" >&2
    echo "ERROR: WP Codebox could not run a raw extra_plugins source that requires Composer autoload preparation." >&2
    echo "  This smoke expects WP Codebox recipe-run to prepare composer.json before activating extra_plugins." >&2
    echo "  Do not add Composer preparation in Homeboy Extensions; update WP Codebox if vendor/autoload.php is missing." >&2
    echo "  Extra plugin source: $EXTRA_PLUGIN_ROOT" >&2
    echo "  Artifacts: $ARTIFACTS_DIR" >&2
    exit "$status"
fi

if [ ! -s "$RESULTS_FILE" ]; then
    echo "ERROR: missing bench results file: $RESULTS_FILE" >&2
    exit 1
fi

jq -e '
    (.scenarios // [] | any(.id == "assert-extra-plugin-autoload"))
' "$RESULTS_FILE" >/dev/null || {
    echo "ERROR: expected extra plugin autoload bench scenario result was not recorded" >&2
    cat "$RESULTS_FILE" >&2
    exit 1
}

echo ""
echo "============================================"
echo "WP Codebox raw extra_plugins Composer autoload smoke PASSED"
echo "============================================"
