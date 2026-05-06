#!/usr/bin/env bash
#
# Validation-dependency DB-touching activation smoke (homeboy-extensions#431).
#
# Asserts the bench runner fires plugin activation AFTER wp-phpunit's
# install.php has created the test tables, by:
#   1. Mounting a dep fixture (bench-db-activation-dep) whose activation
#      hook calls add_option() and get_users() — both of which fatal pre-#431
#      with "no such table: wptests_*".
#   2. Mounting a host fixture (bench-db-activation-host) whose tests/bench/
#      workload reads back the option and asserts both DB writes succeeded.
#   3. Confirming the bench dispatcher exits 0 (no activation fatal) AND
#      the BenchResults envelope reports
#      `dep_activation_option_set_mean=1`, `dep_activation_user_query_succeeded_mean=1`,
#      and `dep_plugins_loaded_fired_mean=1`.
#
# Pre-#431 the dep's activation hook fired during muplugins_loaded — before
# wp-phpunit's install.php had created wptests_options / wptests_users. Any
# real-world activation hook touching the database would fatal under the bench
# runner, blocking Stage 2 of homeboy-extensions#422 (Data Machine end-to-end
# in Playground).
#
# Manual integration test, not part of CI. Run after changes to:
#   - bench-runner-playground.sh
#   - playground-bench-runner.php
#   - scripts/lib/playground-bootstrap.php (especially pg_run_load_deps_stage,
#     pg_run_load_component_stage, and pg_run_activation_stage)
#
# Usage: bash wordpress/scripts/bench/playground-bench-db-activation-smoke.sh
# Exit:  0 = bench runner activates plugins post-install, non-zero = regression

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
HOST_FIXTURE_DIR="${EXTENSION_PATH}/tests/fixtures/bench-db-activation-host"
DEP_FIXTURE_DIR="${EXTENSION_PATH}/tests/fixtures/bench-db-activation-dep"

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

RESULTS_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/bench-db-activation-smoke.XXXXXX")
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
echo "Bench runner DB-touching activation smoke (#431)"
echo "============================================"
echo "Host fixture: $HOST_FIXTURE_DIR"
echo "Dep fixture:  $DEP_FIXTURE_DIR"
echo "Iterations:   2 (per workload)"
echo ""

HOMEBOY_BENCH_RESULTS_FILE="$RESULTS_TMPFILE" \
HOMEBOY_BENCH_ITERATIONS=2 \
HOMEBOY_COMPONENT_ID=bench-db-activation-host \
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

# --- Assertion: workload scenario present (no activation fatal) ---
scenario='.scenarios[] | select(.id == "assert-dep-db-activation")'
source=$(jq -r "$scenario | .source // \"missing\"" "$RESULTS_TMPFILE")
if [ "$source" != "in_tree" ]; then
    echo "ERROR: assert-dep-db-activation workload missing from results envelope (source=$source)" >&2
    echo "       The dep's DB-touching activation hook likely fataled — see homeboy-extensions#431." >&2
    exit 1
fi
echo "✓ assert-dep-db-activation scenario present"

# --- Assertion: dep plugins_loaded callback fired (mean across iterations == 1) ---
fired=$(jq -r "$scenario | .metrics.dep_plugins_loaded_fired_mean // \"missing\"" "$RESULTS_TMPFILE")
if [ "$fired" != "1" ]; then
    echo "ERROR: dep_plugins_loaded_fired_mean expected 1, got $fired" >&2
    echo "       homeboy-extensions#426/#427 regression: dep entry file loaded after plugins_loaded fired." >&2
    exit 1
fi
echo "✓ dep plugins_loaded callback fired"

# --- Assertion: dep's add_option() activation write succeeded ---
option_set=$(jq -r "$scenario | .metrics.dep_activation_option_set_mean // \"missing\"" "$RESULTS_TMPFILE")
if [ "$option_set" != "1" ]; then
    echo "ERROR: dep_activation_option_set_mean expected 1, got $option_set" >&2
    echo "       homeboy-extensions#431 regression: dep activation hook fired before wp-phpunit" >&2
    echo "       created wptests_options. Verify pg_run_activation_stage runs AFTER pg_run_install_stage." >&2
    exit 1
fi
echo "✓ dep activation hook wrote to wptests_options after install"

# --- Assertion: dep's get_users() activation read succeeded ---
user_query=$(jq -r "$scenario | .metrics.dep_activation_user_query_succeeded_mean // \"missing\"" "$RESULTS_TMPFILE")
if [ "$user_query" != "1" ]; then
    echo "ERROR: dep_activation_user_query_succeeded_mean expected 1, got $user_query" >&2
    echo "       homeboy-extensions#431 regression: get_users() during dep activation could not query" >&2
    echo "       wptests_users. Activation must run AFTER pg_run_install_stage." >&2
    exit 1
fi
echo "✓ dep activation hook read from wptests_users after install"

# --- Assertion: bootstrap stage timings include the new activation stage ---
activation_ms=$(jq -r '.scenarios[] | select(.id == "__bootstrap") | .metrics.activation_ms // "missing"' "$RESULTS_TMPFILE")
if [ "$activation_ms" = "missing" ]; then
    echo "ERROR: __bootstrap scenario is missing activation_ms — pg_run_activation_stage was not called or did not complete." >&2
    exit 1
fi
echo "✓ __bootstrap synthetic scenario reports activation_ms ($activation_ms ms)"

# --- Assertion: bootstrap stage timings still emitted with __bootstrap first ---
first_id=$(jq -r '.scenarios[0].id' "$RESULTS_TMPFILE")
if [ "$first_id" != "__bootstrap" ]; then
    echo "ERROR: expected scenarios[0].id == '__bootstrap', got '$first_id'" >&2
    exit 1
fi
echo "✓ __bootstrap synthetic scenario still first"

echo ""
echo "============================================"
echo "✓ Bench DB-touching activation smoke PASSED"
echo "============================================"
