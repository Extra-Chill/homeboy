#!/usr/bin/env bash
set -euo pipefail

# WP Codebox-backed minimal WordPress bench runner.
#
# This first #698 slice covers in-tree `tests/bench/*.php` workloads. The
# top-level bench router keeps complex Playground-only features on the legacy
# runner until their WP Codebox equivalents are ported explicitly.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${SCRIPT_DIR}/../lib/resolve-context.sh}"
FAILURE_TRAP_HELPER="${HOMEBOY_RUNTIME_FAILURE_TRAP:-}"
BENCH_HELPER_SH="${HOMEBOY_RUNTIME_BENCH_HELPER_SH:-${HOME}/.homeboy/runtime/bench-helper.sh}"
PLAYGROUND_RESULTS_ARTIFACTS_HELPER="${SCRIPT_DIR}/playground-results-artifacts.sh"

# shellcheck source=/dev/null
source "$RESOLVE_CONTEXT_HELPER"
homeboy_resolve_context --component-alias PLUGIN_PATH

# shellcheck source=/dev/null
if [ -n "$FAILURE_TRAP_HELPER" ] && [ -f "$FAILURE_TRAP_HELPER" ]; then
    source "$FAILURE_TRAP_HELPER"
    homeboy_init_failure_trap
else
    FAILED_STEP=""
    FAILURE_OUTPUT=""
fi

# shellcheck source=/dev/null
if [ -f "$BENCH_HELPER_SH" ]; then
    source "$BENCH_HELPER_SH"
else
    echo "ERROR: Homeboy bench helper not found at ${BENCH_HELPER_SH}" >&2
    exit 2
fi
# shellcheck source=playground-results-artifacts.sh
source "$PLAYGROUND_RESULTS_ARTIFACTS_HELPER"

if [ -n "${COMPONENT_ID:-}" ]; then
    PLUGIN_SLUG="$COMPONENT_ID"
else
    PLUGIN_SLUG="$(basename "$PLUGIN_PATH")"
    COMPONENT_ID="$PLUGIN_SLUG"
fi

WP_CODEBOX_BIN="${HOMEBOY_WP_CODEBOX_BIN:-}"
if [ -z "$WP_CODEBOX_BIN" ] && [ -n "${HOMEBOY_SETTINGS_JSON:-}" ] && [ "${HOMEBOY_SETTINGS_JSON}" != "{}" ]; then
    WP_CODEBOX_BIN=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -r '.wp_codebox_bin // empty' 2>/dev/null || true)
fi
WP_CODEBOX_BIN="${WP_CODEBOX_BIN:-wp-codebox}"
if [ "$WP_CODEBOX_BIN" = "wp-codebox" ] && ! command -v wp-codebox >/dev/null 2>&1; then
    echo "Error: wp-codebox not found; set HOMEBOY_WP_CODEBOX_BIN, settings wp_codebox_bin, or install wp-codebox." >&2
    FAILED_STEP="WP Codebox CLI setup"
    exit 1
fi

BENCH_DIR="${PLUGIN_PATH}/tests/bench"
if [ ! -d "$BENCH_DIR" ]; then
    echo "Warning: No bench workload directory found at ${BENCH_DIR}" >&2
    exit 0
fi

PLAYGROUND_WORDPRESS_VERSION="7.0"
if [ -n "${HOMEBOY_SETTINGS_JSON:-}" ] && [ "${HOMEBOY_SETTINGS_JSON}" != "{}" ]; then
    extracted=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -r '.wp_codebox_wordpress_version // .playground_wordpress_version // empty' 2>/dev/null || true)
    [ -n "$extracted" ] && [ "$extracted" != "null" ] && PLAYGROUND_WORDPRESS_VERSION="$extracted"
fi

ITERATIONS="${HOMEBOY_BENCH_ITERATIONS:-3}"
WARMUP_ITERATIONS="${HOMEBOY_BENCH_WARMUP_ITERATIONS:-1}"
RESULTS_FILE="${HOMEBOY_BENCH_RESULTS_FILE:-${PLUGIN_PATH}/.pg-bench-results.json}"

ARTIFACTS_DIR="${HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR:-}"
if [ -z "$ARTIFACTS_DIR" ] && [ -n "${HOMEBOY_SETTINGS_JSON:-}" ] && [ "${HOMEBOY_SETTINGS_JSON}" != "{}" ]; then
    ARTIFACTS_DIR=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -r '.wp_codebox_artifacts_dir // empty' 2>/dev/null || true)
fi
if [ -z "$ARTIFACTS_DIR" ]; then
    ARTIFACTS_DIR=$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wp-codebox-bench-artifacts.XXXXXX")
fi

wp_codebox_command=("$WP_CODEBOX_BIN")
case "$WP_CODEBOX_BIN" in
    *.js)
        wp_codebox_command=(node "$WP_CODEBOX_BIN")
        ;;
esac

echo "Running bench workloads via WP Codebox..."
echo "  Plugin: ${PLUGIN_SLUG} (${PLUGIN_PATH})"
echo "  Backend: wp-codebox (WordPress Playground runtime)"

WP_CODEBOX_TMPFILE=$(mktemp)
set +e
"${wp_codebox_command[@]}" bench-run \
    --component "$PLUGIN_PATH" \
    --component-id "$COMPONENT_ID" \
    --plugin-slug "$PLUGIN_SLUG" \
    --iterations "$ITERATIONS" \
    --warmup "$WARMUP_ITERATIONS" \
    --wp "$PLAYGROUND_WORDPRESS_VERSION" \
    --artifacts "$ARTIFACTS_DIR" \
    --json \
    > "$WP_CODEBOX_TMPFILE" 2>&1
wp_codebox_exit=$?
set -e

if [ $wp_codebox_exit -ne 0 ]; then
    cat "$WP_CODEBOX_TMPFILE" >&2
    FAILED_STEP="WP Codebox bench run"
    exit $wp_codebox_exit
fi

if ! jq -e '.success == true and (.benchResults | type == "object")' "$WP_CODEBOX_TMPFILE" >/dev/null 2>&1; then
    cat "$WP_CODEBOX_TMPFILE" >&2
    FAILED_STEP="WP Codebox bench results parse"
    exit 1
fi

mkdir -p "$(dirname "$RESULTS_FILE")"
jq '.benchResults' "$WP_CODEBOX_TMPFILE" > "$RESULTS_FILE"

homeboy_wordpress_emit_playground_results_artifacts "$RESULTS_FILE"

rm -f "$WP_CODEBOX_TMPFILE"
