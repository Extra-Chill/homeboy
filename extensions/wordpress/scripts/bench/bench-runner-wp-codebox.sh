#!/usr/bin/env bash
set -euo pipefail

# WP Codebox-backed WordPress bench runner.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${SCRIPT_DIR}/../lib/resolve-context.sh}"
FAILURE_TRAP_HELPER="${HOMEBOY_RUNTIME_FAILURE_TRAP:-}"
BENCH_HELPER_SH="${HOMEBOY_RUNTIME_BENCH_HELPER_SH:-${HOME}/.homeboy/runtime/bench-helper.sh}"
PLAYGROUND_RESULTS_ARTIFACTS_HELPER="${SCRIPT_DIR}/playground-results-artifacts.sh"
DEPENDENCY_HELPER="${HOMEBOY_WORDPRESS_DEPENDENCY_HELPER:-${SCRIPT_DIR}/../lib/validation-dependencies.sh}"
BENCH_BROWSER_TARGET_HELPER="${SCRIPT_DIR}/browser-target.sh"

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
# shellcheck source=../lib/validation-dependencies.sh
if [ -f "$DEPENDENCY_HELPER" ]; then
    source "$DEPENDENCY_HELPER"
fi
# shellcheck source=browser-target.sh
source "$BENCH_BROWSER_TARGET_HELPER"

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

settings_json="${HOMEBOY_SETTINGS_JSON:-}"
[ -n "$settings_json" ] || settings_json="{}"

if printf '%s' "$settings_json" | jq -e '((.playground_blueprint // {}) | length > 0) or ((.playground_scenario_manifests // .scenario_manifests // []) | length > 0) or ((.bench_site_mode // "fresh") == "installed")' >/dev/null 2>&1; then
    echo "Error: this bench configuration uses Playground-only bootstrap features that no longer dispatch to the legacy runner." >&2
    echo "       Port the setting to wp-codebox first: playground_blueprint, scenario manifests, or bench_site_mode=installed." >&2
    FAILED_STEP="WP Codebox bench configuration"
    exit 1
fi

PLAYGROUND_WORDPRESS_VERSION="7.0"
if [ "$settings_json" != "{}" ]; then
    extracted=$(printf '%s' "$settings_json" | jq -r '.wp_codebox_wordpress_version // .playground_wordpress_version // empty' 2>/dev/null || true)
    [ -n "$extracted" ] && [ "$extracted" != "null" ] && PLAYGROUND_WORDPRESS_VERSION="$extracted"
fi

ITERATIONS="${HOMEBOY_BENCH_ITERATIONS:-3}"
WARMUP_ITERATIONS="${HOMEBOY_BENCH_WARMUP_ITERATIONS:-}"
if [ -z "$WARMUP_ITERATIONS" ] && [ "$settings_json" != "{}" ]; then
    extracted=$(printf '%s' "$settings_json" | jq -r '.bench_warmup_iterations // empty' 2>/dev/null || true)
    [ -n "$extracted" ] && [ "$extracted" != "null" ] && WARMUP_ITERATIONS="$extracted"
fi
WARMUP_ITERATIONS="${WARMUP_ITERATIONS:-1}"
RESULTS_FILE="${HOMEBOY_BENCH_RESULTS_FILE:-${PLUGIN_PATH}/.pg-bench-results.json}"

ARTIFACTS_DIR="${HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR:-}"
if [ -z "$ARTIFACTS_DIR" ] && [ "$settings_json" != "{}" ]; then
    ARTIFACTS_DIR=$(printf '%s' "$settings_json" | jq -r '.wp_codebox_artifacts_dir // empty' 2>/dev/null || true)
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

if type homeboy_export_validation_dependency_paths &>/dev/null; then
    homeboy_export_validation_dependency_paths "$PLUGIN_PATH"
fi
DEPENDENCY_PATHS="${HOMEBOY_WORDPRESS_DEPENDENCY_PATHS:-}"

WP_CONFIG_DEFINES_JSON="{}"
BENCH_ENV_JSON="{}"
PLAYGROUND_WORKLOADS_JSON="[]"
if [ "$settings_json" != "{}" ]; then
    WP_CONFIG_DEFINES_JSON=$(printf '%s' "$settings_json" | jq -c '.wp_config_defines // {}' 2>/dev/null || echo "{}")
    BENCH_ENV_JSON=$(printf '%s' "$settings_json" | jq -c '.bench_env // {}' 2>/dev/null || echo "{}")
    PLAYGROUND_WORKLOADS_JSON=$(printf '%s' "$settings_json" | jq -c '.playground_workloads // []' 2>/dev/null || echo "[]")
fi

MOUNT_ARGS=()
DEPENDENCY_ARGS=()
if [ -n "$DEPENDENCY_PATHS" ]; then
    while IFS= read -r dep_path; do
        [ -n "$dep_path" ] || continue
        dep_slug="$(homeboy_get_validation_dependency_slug "$dep_path" || basename "$dep_path")"
        DEPENDENCY_ARGS+=("--dependency" "${dep_path}:${dep_slug}")
    done <<< "$DEPENDENCY_PATHS"
fi

PLUGIN_DB_PHP="${PLUGIN_PATH}/db.php"
if [ -f "$PLUGIN_DB_PHP" ]; then
    MOUNT_ARGS+=("--mount" "${PLUGIN_DB_PHP}:/wordpress/wp-content/db.php")
fi

PLAYGROUND_FILE_MOUNTS_JSON="[]"
if [ "$settings_json" != "{}" ]; then
    PLAYGROUND_FILE_MOUNTS_JSON=$(printf '%s' "$settings_json" | jq -c '.playground_file_mounts // []' 2>/dev/null || echo "[]")
fi
if printf '%s' "$PLAYGROUND_FILE_MOUNTS_JSON" | jq -e 'type == "array" and length > 0' >/dev/null 2>&1; then
    while IFS= read -r mount_json; do
        [ -n "$mount_json" ] || continue
        mount_from=$(printf '%s' "$mount_json" | jq -r '.from // empty')
        mount_to=$(printf '%s' "$mount_json" | jq -r '.to // empty')
        mount_dependency=$(printf '%s' "$mount_json" | jq -r '.from_dependency // empty')
        if [ -z "$mount_from" ] || [ -z "$mount_to" ] || [[ "$mount_from" = /* ]] || [[ "$mount_from" == *..* ]] || [[ "$mount_to" != /* ]]; then
            echo "Error: playground_file_mounts entries require relative 'from' and absolute 'to' paths." >&2
            FAILED_STEP="WP Codebox file mount setup"
            exit 1
        fi
        mount_root="$PLUGIN_PATH"
        if [ -n "$mount_dependency" ]; then
            mount_root=""
            if [ -n "$DEPENDENCY_PATHS" ]; then
                while IFS= read -r dep_path; do
                    [ -n "$dep_path" ] || continue
                    dep_slug="$(homeboy_get_validation_dependency_slug "$dep_path" || basename "$dep_path")"
                    if [ "$dep_slug" = "$mount_dependency" ] || [ "$(basename "$dep_path")" = "$mount_dependency" ]; then
                        mount_root="$dep_path"
                        break
                    fi
                done <<< "$DEPENDENCY_PATHS"
            fi
        fi
        if [ -z "$mount_root" ]; then
            echo "Error: playground_file_mounts dependency not found: $mount_dependency" >&2
            FAILED_STEP="WP Codebox file mount setup"
            exit 1
        fi
        mount_host="${mount_root}/${mount_from}"
        if [ ! -f "$mount_host" ]; then
            echo "Error: playground_file_mounts source file not found: $mount_host" >&2
            FAILED_STEP="WP Codebox file mount setup"
            exit 1
        fi
        MOUNT_ARGS+=("--mount" "${mount_host}:${mount_to}")
    done < <(printf '%s' "$PLAYGROUND_FILE_MOUNTS_JSON" | jq -c '.[]')
fi

SHARED_STATE_HOST="${HOMEBOY_BENCH_SHARED_STATE:-}"
if [ -n "$SHARED_STATE_HOST" ]; then
    mkdir -p "$SHARED_STATE_HOST"
    MOUNT_ARGS+=("--mount" "${SHARED_STATE_HOST}:/bench-shared-state")
    BENCH_ENV_JSON=$(jq -nc --argjson env "$BENCH_ENV_JSON" --arg shared "/bench-shared-state" --arg instance "${HOMEBOY_BENCH_INSTANCE_ID:-0}" --arg concurrency "${HOMEBOY_BENCH_CONCURRENCY:-1}" '$env + {HOMEBOY_BENCH_SHARED_STATE: $shared, HOMEBOY_BENCH_INSTANCE_ID: $instance, HOMEBOY_BENCH_CONCURRENCY: $concurrency}')
    WP_CONFIG_DEFINES_JSON=$(jq -nc --argjson defines "$WP_CONFIG_DEFINES_JSON" --arg shared "/bench-shared-state" --arg instance "${HOMEBOY_BENCH_INSTANCE_ID:-0}" --arg concurrency "${HOMEBOY_BENCH_CONCURRENCY:-1}" '$defines + {HOMEBOY_BENCH_SHARED_STATE: $shared, HOMEBOY_BENCH_INSTANCE_ID: $instance, HOMEBOY_BENCH_CONCURRENCY: $concurrency}')
fi

if ! homeboy_wordpress_emit_browser_target "$settings_json" "$SHARED_STATE_HOST" "$COMPONENT_ID" "$PLUGIN_SLUG" "fresh"; then
    FAILED_STEP="Browser bench target setup"
    exit 1
fi

BENCH_DIR="${PLUGIN_PATH}/tests/bench"
if [ ! -d "$BENCH_DIR" ] && ! printf '%s' "$PLAYGROUND_WORKLOADS_JSON" | jq -e 'type == "array" and length > 0' >/dev/null 2>&1; then
    echo "Warning: No bench workloads found for ${PLUGIN_PATH}" >&2
    if [ -n "${HOMEBOY_BENCH_RESULTS_FILE:-}" ]; then
        homeboy_write_empty_bench_results "$COMPONENT_ID" 0 "$RESULTS_FILE"
        homeboy_wordpress_emit_playground_results_artifacts "$RESULTS_FILE"
    fi
    exit 0
fi

echo "Running bench workloads via WP Codebox..."
echo "  Plugin: ${PLUGIN_SLUG} (${PLUGIN_PATH})"
echo "  Backend: wp-codebox (WordPress Playground runtime)"

WP_CODEBOX_TMPFILE=$(mktemp)
set +e
"${wp_codebox_command[@]}" bench-run \
    --component "$PLUGIN_PATH" \
    --component-id "$COMPONENT_ID" \
    --plugin-slug "$PLUGIN_SLUG" \
    "${DEPENDENCY_ARGS[@]}" \
    "${MOUNT_ARGS[@]}" \
    --env-json "$BENCH_ENV_JSON" \
    --wp-config-defines-json "$WP_CONFIG_DEFINES_JSON" \
    --workloads-json "$PLAYGROUND_WORKLOADS_JSON" \
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
