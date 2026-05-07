#!/usr/bin/env bash
#
# WP-CLI plugin install workload smoke test (homeboy-extensions#454).
#
# Configured workload `wp-cli` steps must support the bundled WP-CLI
# command surface so workloads can prepare WordPress.org plugin
# dependencies before subsequent steps. Before #454 the runner only
# exposed wp-cli/wp-cli (the dispatcher) without any of the bundled
# command packages, so `wp plugin install woocommerce --activate` failed
# with "'plugin' is not a registered wp command".
#
# This smoke runs a configured workload with two steps:
#   1. wp-cli: `wp plugin install hello-dolly --activate`
#   2. php:    asserts hello-dolly is active and its functions are
#              loaded into the running Playground PHP process.
#
# Asserts the BenchResults envelope reports the workload as `source: config`,
# both metrics emit `1`, and metadata.active_plugins contains
# hello-dolly/hello.php — proving the wp-cli step actually installed
# and activated a real wordpress.org plugin from inside Playground.
#
# `hello-dolly` is the test plugin (small, single-file, real wp.org slug).
# WooCommerce is the production-side caller in the issue, but exercising
# any wordpress.org plugin proves the same contract — the bench harness
# is plugin-agnostic.
#
# Usage: bash wordpress/scripts/bench/playground-bench-wp-cli-plugin-install-smoke.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
FIXTURE_DIR="${EXTENSION_PATH}/tests/fixtures/playground-workloads-wp-cli-plugin"

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

# Sanity-check that the bundled WP-CLI command packages this PR adds are
# actually installed in vendor/. If a follow-up regresses composer.json
# the smoke would fail later inside Playground with a confusing
# "'plugin' is not a registered wp command" — fail fast here with a clear
# pointer instead.
if [ ! -f "${EXTENSION_PATH}/vendor/wp-cli/extension-command/extension-command.php" ]; then
    echo "ERROR: wp-cli/extension-command not installed in vendor/." >&2
    echo "This smoke proves homeboy-extensions#454: configured workload wp-cli" >&2
    echo "steps need the bundled WP-CLI command surface (wp plugin, wp theme, ...)." >&2
    echo "Run: cd ${EXTENSION_PATH} && composer install" >&2
    exit 1
fi

if ! command -v jq >/dev/null 2>&1; then
    echo "ERROR: jq required for JSON assertions in this smoke." >&2
    echo "Install: brew install jq (macOS) or your package manager." >&2
    exit 1
fi

RESULTS_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/bench-wp-cli-plugin-install-smoke.XXXXXX")

cleanup() {
    rm -f "$RESULTS_TMPFILE"
}
trap cleanup EXIT

# Single configured workload with two steps:
#   1. wp-cli step installs + activates hello-dolly from wordpress.org.
#   2. php step asserts the install + activation actually took effect.
#
# Both steps run inside the same Playground PHP process, so step 2 sees
# the option/active_plugins state step 1 mutated.
SETTINGS_JSON=$(cat <<'JSON'
{
  "playground_workloads": [
    {
      "id": "wp-cli-plugin-install",
      "label": "WP-CLI plugin install + activate",
      "run": [
        {
          "type": "wp-cli",
          "command": "wp plugin install hello-dolly --activate"
        },
        {
          "type": "php",
          "file": "workloads/assert-plugin-active.php"
        }
      ]
    }
  ]
}
JSON
)

echo "============================================"
echo "Playground bench wp-cli plugin install smoke test"
echo "============================================"
echo "Fixture:    $FIXTURE_DIR"
echo "Iterations: 1 (workload installs from wp.org; keep the bench cheap)"
echo ""

HOMEBOY_BENCH_RESULTS_FILE="$RESULTS_TMPFILE" \
HOMEBOY_BENCH_ITERATIONS=1 \
HOMEBOY_COMPONENT_ID=playground-workloads-wp-cli-plugin \
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

scenario_count=$(jq -r '.scenarios | length' "$RESULTS_TMPFILE")
if [ "$scenario_count" -ne 2 ]; then
    echo "ERROR: expected 2 scenarios (__bootstrap + configured workload), got $scenario_count" >&2
    exit 1
fi
echo "✓ scenarios length == 2 (__bootstrap + configured workload)"

scenario='.scenarios[] | select(.id == "wp-cli-plugin-install")'

source=$(jq -r "$scenario | .source" "$RESULTS_TMPFILE")
if [ "$source" != "config" ]; then
    echo "ERROR: expected source=config, got $source" >&2
    exit 1
fi
echo "✓ configured workload source emitted"

active=$(jq -r "$scenario | .metrics.plugin_active_mean // \"missing\"" "$RESULTS_TMPFILE")
if [ "$active" != "1" ]; then
    echo "ERROR: plugin_active_mean expected 1, got $active" >&2
    echo "       wp-cli step did not activate hello-dolly inside Playground." >&2
    exit 1
fi
echo "✓ wp-cli step activated hello-dolly (plugin_active_mean=1)"

loaded=$(jq -r "$scenario | .metrics.plugin_file_loaded_mean // \"missing\"" "$RESULTS_TMPFILE")
if [ "$loaded" != "1" ]; then
    echo "ERROR: plugin_file_loaded_mean expected 1, got $loaded" >&2
    echo "       hello-dolly was activated but its functions are not loaded." >&2
    exit 1
fi
echo "✓ hello-dolly main file loaded into the workload PHP process"

active_plugin_match=$(jq -r "$scenario | .metadata.active_plugins | index(\"hello-dolly/hello.php\") // \"missing\"" "$RESULTS_TMPFILE")
if [ "$active_plugin_match" = "missing" ] || [ "$active_plugin_match" = "null" ]; then
    echo "ERROR: hello-dolly/hello.php not present in active_plugins metadata" >&2
    exit 1
fi
echo "✓ active_plugins metadata contains hello-dolly/hello.php"

echo ""
echo "============================================"
echo "✓ WP-CLI plugin install smoke test PASSED"
echo "============================================"
