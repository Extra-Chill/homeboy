#!/usr/bin/env bash
#
# Validation-dependency `plugins_loaded` smoke (homeboy-extensions#426).
#
# Asserts the bench runner loads validation-dependency entry files BEFORE
# wp-settings.php fires `plugins_loaded`, by:
#   1. Mounting a dep fixture (bench-plugins-loaded-dep) whose plugins_loaded
#      callback flips a global flag and registers an Abilities API ability.
#   2. Mounting a host fixture (bench-plugins-loaded-host) whose tests/bench/
#      workload throws if the global is unset.
#   3. Confirming the bench dispatcher exits 0 (workload didn't throw) AND
#      the BenchResults envelope reports `dep_plugins_loaded_fired_mean=1`,
#      `dep_ability_registered_mean=1`, and `dep_plugin_active_mean=1`.
#
# Pre-#426 the workload threw because the dep's plugins_loaded callback
# never ran — its file was required AFTER plugins_loaded had already fired.
#
# Manual integration test, not part of CI. Run after changes to:
#   - bench-runner-playground.sh
#   - playground-bench-runner.php
#   - scripts/lib/playground-bootstrap.php
#
# Usage: bash wordpress/scripts/bench/playground-bench-dep-plugins-loaded-smoke.sh
# Exit:  0 = bench runner loads deps before plugins_loaded, non-zero = regression

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
HOST_FIXTURE_DIR="${EXTENSION_PATH}/tests/fixtures/bench-plugins-loaded-host"
DEP_FIXTURE_DIR="${EXTENSION_PATH}/tests/fixtures/bench-plugins-loaded-dep"

if [ ! -d "$HOST_FIXTURE_DIR" ]; then
    echo "ERROR: host fixture not found at $HOST_FIXTURE_DIR" >&2
    exit 1
fi

if [ ! -d "$DEP_FIXTURE_DIR" ]; then
    echo "ERROR: dep fixture not found at $DEP_FIXTURE_DIR" >&2
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
    echo "Install: brew install jq (macOS) or your package manager." >&2
    exit 1
fi

RESULTS_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/bench-dep-plugins-loaded-smoke.XXXXXX")
cleanup() {
    rm -f "$RESULTS_TMPFILE"
}
trap cleanup EXIT

# Pass the dep fixture as an absolute-path validation dependency. The
# validation-dependencies.sh resolver hits the direct-path branch first, so
# this avoids the homeboy / git-clone fallbacks (this is a self-contained
# fixture that lives inside the extension repo).
SETTINGS_JSON=$(jq -n --arg dep "$DEP_FIXTURE_DIR" '{
  "validation_dependencies": [$dep]
}')

echo "============================================"
echo "Bench runner dep plugins_loaded smoke (#426)"
echo "============================================"
echo "Host fixture: $HOST_FIXTURE_DIR"
echo "Dep fixture:  $DEP_FIXTURE_DIR"
echo "Iterations:   2 (per workload)"
echo ""

HOMEBOY_BENCH_RESULTS_FILE="$RESULTS_TMPFILE" \
HOMEBOY_BENCH_ITERATIONS=2 \
HOMEBOY_COMPONENT_ID=bench-plugins-loaded-host \
HOMEBOY_COMPONENT_PATH="$HOST_FIXTURE_DIR" \
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

# --- Assertion: workload scenario present (bench runner didn't fail) ---
scenario='.scenarios[] | select(.id == "assert-dep-plugins-loaded")'
source=$(jq -r "$scenario | .source // \"missing\"" "$RESULTS_TMPFILE")
if [ "$source" != "in_tree" ]; then
    echo "ERROR: assert-dep-plugins-loaded workload missing from results envelope (source=$source)" >&2
    exit 1
fi
echo "✓ assert-dep-plugins-loaded scenario present"

# --- Assertion: dep plugins_loaded callback fired (mean across iterations == 1) ---
fired=$(jq -r "$scenario | .metrics.dep_plugins_loaded_fired_mean // \"missing\"" "$RESULTS_TMPFILE")
if [ "$fired" != "1" ]; then
    echo "ERROR: dep_plugins_loaded_fired_mean expected 1, got $fired" >&2
    echo "       This is the homeboy-extensions#426 regression — the bench runner" >&2
    echo "       required the dep entry file AFTER wp-settings.php fired plugins_loaded." >&2
    exit 1
fi
echo "✓ dep plugins_loaded callback fired"

# --- Assertion: dep registered an ability via wp_abilities_api_init ---
registered=$(jq -r "$scenario | .metrics.dep_ability_registered_mean // \"missing\"" "$RESULTS_TMPFILE")
if [ "$registered" != "1" ]; then
    echo "ERROR: dep_ability_registered_mean expected 1, got $registered" >&2
    echo "       The dep loaded but its abilities never reached the registry." >&2
    exit 1
fi
echo "✓ dep ability registered with the Abilities API"

# --- Assertion: dep is visible through WordPress active plugin state ---
active=$(jq -r "$scenario | .metrics.dep_plugin_active_mean // \"missing\"" "$RESULTS_TMPFILE")
if [ "$active" != "1" ]; then
    echo "ERROR: dep_plugin_active_mean expected 1, got $active" >&2
    echo "       The dep loaded, but WordPress still reports it inactive." >&2
    exit 1
fi
echo "✓ dep appears in WordPress active plugin state"

active_plugin_match=$(jq -r "$scenario | .metadata.active_plugins | index(\"bench-plugins-loaded-dep/bench-plugins-loaded-dep.php\") // \"missing\"" "$RESULTS_TMPFILE")
if [ "$active_plugin_match" = "missing" ] || [ "$active_plugin_match" = "null" ]; then
    echo "ERROR: bench-plugins-loaded-dep missing from active_plugins metadata" >&2
    exit 1
fi
echo "✓ active_plugins metadata contains validation dependency"

# --- Assertion: bootstrap stage timings still emitted ---
first_id=$(jq -r '.scenarios[0].id' "$RESULTS_TMPFILE")
if [ "$first_id" != "__bootstrap" ]; then
    echo "ERROR: expected scenarios[0].id == '__bootstrap', got '$first_id'" >&2
    exit 1
fi
echo "✓ __bootstrap synthetic scenario still first"

echo ""
echo "============================================"
echo "✓ Bench dep plugins_loaded smoke PASSED"
echo "============================================"
