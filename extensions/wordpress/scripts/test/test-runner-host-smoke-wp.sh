#!/usr/bin/env bash
set -euo pipefail

# Real-WordPress smoke runner for components that carry standalone *-smoke.php
# scripts. Unlike the bare-PHP host-smoke backend, this boots real WordPress in
# the WP Codebox sandbox, mounts the plugin (and its validation dependencies and
# db.php drop-in) into wp-content/plugins, and runs each smoke via the
# wordpress.run-php recipe step with WordPress loaded.
#
# Because real WP functions (wp_json_encode, sanitize_*, apply_filters, ...) exist
# in this environment, smokes no longer need to define their own
# `if (!function_exists('wp_...'))` shims; any such guards become harmless dead
# code. This is the dependency-resolution layer doing its job instead of every
# smoke faking WordPress.
#
# It preserves the host-smoke contract: HOST_SMOKE_BEGIN / HOST_SMOKE_OK /
# HOST_SMOKE_FAIL / HOST_SMOKE_SUMMARY markers and the same file-selection env
# vars (HOMEBOY_WORDPRESS_HOST_SMOKE_FILE / _FILES).

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${SCRIPT_DIR}/../lib/resolve-context.sh}"
WP_CODEBOX_PATHS_HELPER="${HOMEBOY_RUNTIME_WP_CODEBOX_PATHS:-${SCRIPT_DIR}/../lib/wp-codebox-paths.sh}"
VALIDATION_DEPS_HELPER="${HOMEBOY_RUNTIME_VALIDATION_DEPENDENCIES:-${SCRIPT_DIR}/../lib/validation-dependencies.sh}"

# shellcheck source=/dev/null
source "$RESOLVE_CONTEXT_HELPER"
# shellcheck source=/dev/null
source "$WP_CODEBOX_PATHS_HELPER"
# Validation-dependency discovery is optional; only source it if present so this
# backend still runs for components without the helper.
if [ -f "$VALIDATION_DEPS_HELPER" ]; then
    # shellcheck source=/dev/null
    source "$VALIDATION_DEPS_HELPER"
fi

homeboy_resolve_context --component-alias PLUGIN_PATH

TEST_DIR="${PLUGIN_PATH}/tests"
PLUGIN_SLUG="${COMPONENT_ID:-$(basename "$PLUGIN_PATH")}"
TARGET_SMOKE_FILE="${HOMEBOY_WORDPRESS_HOST_SMOKE_FILE:-}"
TARGET_SMOKE_FILES="${HOMEBOY_WORDPRESS_HOST_SMOKE_FILES:-}"

homeboy_wordpress_host_smoke_abs() {
    local raw_path="$1"
    local abs_path

    if [ "${raw_path#/}" != "$raw_path" ]; then
        abs_path="$raw_path"
    else
        abs_path="${PLUGIN_PATH}/${raw_path}"
    fi

    if [ ! -f "$abs_path" ]; then
        echo "ERROR: requested host smoke file not found: ${raw_path}" >&2
        return 2
    fi

    case "$abs_path" in
        "${PLUGIN_PATH}"/tests/*-smoke.php)
            printf '%s\n' "$abs_path"
            ;;
        *)
            echo "ERROR: requested host smoke file must match tests/**/*-smoke.php: ${raw_path}" >&2
            return 2
            ;;
    esac
}

homeboy_wordpress_resolve_wp_codebox_bin() {
    local bin="${HOMEBOY_WP_CODEBOX_BIN:-}"

    if [ -z "$bin" ] && [ -n "${HOMEBOY_SETTINGS_JSON:-}" ] && [ "${HOMEBOY_SETTINGS_JSON}" != "{}" ]; then
        bin=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -r '.wp_codebox_bin // empty' 2>/dev/null || true)
    fi

    bin="${bin:-wp-codebox}"
    if [ "$bin" = "wp-codebox" ] && ! command -v wp-codebox >/dev/null 2>&1; then
        echo "Error: wp-codebox not found; set HOMEBOY_WP_CODEBOX_BIN, settings wp_codebox_bin, or install wp-codebox." >&2
        return 1
    fi

    printf '%s\n' "$bin"
}

homeboy_wordpress_smoke_wp_version() {
    local version=""
    if [ -n "${HOMEBOY_SETTINGS_JSON:-}" ] && [ "${HOMEBOY_SETTINGS_JSON}" != "{}" ]; then
        local extracted
        extracted=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -r '.wp_codebox_wordpress_version // empty' 2>/dev/null || true)
        [ -n "$extracted" ] && [ "$extracted" != "null" ] && version="$extracted"
    fi
    printf '%s\n' "$version"
}

# Build the JSON array of recipe mounts: the plugin, its validation
# dependencies, and the db.php drop-in if present. WordPress loads all mounted
# plugins so the smoke's `require` paths resolve and WP core functions exist.
homeboy_wordpress_smoke_recipe_mounts() {
    local mounts_json='[]'
    local plugin_source
    plugin_source="$(homeboy_wp_codebox_resolve_mount_path "$PLUGIN_PATH")"
    mounts_json=$(jq -nc --argjson mounts "$mounts_json" --arg source "$plugin_source" --arg target "/wordpress/wp-content/plugins/${PLUGIN_SLUG}" '$mounts + [{source: $source, target: $target, mode: "readonly"}]')

    if type homeboy_export_validation_dependency_paths >/dev/null 2>&1; then
        homeboy_export_validation_dependency_paths "$PLUGIN_PATH" >/dev/null 2>&1 || true
    fi
    if [ -n "${HOMEBOY_WORDPRESS_DEPENDENCY_PATHS:-}" ]; then
        local dep_path dep_slug dep_source
        while IFS= read -r dep_path; do
            [ -n "$dep_path" ] || continue
            [ -d "$dep_path" ] || continue
            if type homeboy_get_validation_dependency_slug >/dev/null 2>&1; then
                dep_slug="$(homeboy_get_validation_dependency_slug "$dep_path" || basename "$dep_path")"
            else
                dep_slug="$(basename "$dep_path")"
            fi
            dep_source="$(homeboy_wp_codebox_resolve_mount_path "$dep_path")"
            mounts_json=$(jq -nc --argjson mounts "$mounts_json" --arg source "$dep_source" --arg target "/wordpress/wp-content/plugins/${dep_slug}" '$mounts + [{source: $source, target: $target, mode: "readonly"}]')
        done <<< "$HOMEBOY_WORDPRESS_DEPENDENCY_PATHS"
    fi

    if [ -f "${PLUGIN_PATH}/db.php" ]; then
        local db_source
        db_source="$(homeboy_wp_codebox_resolve_mount_path "${PLUGIN_PATH}/db.php")"
        mounts_json=$(jq -nc --argjson mounts "$mounts_json" --arg source "$db_source" --arg target "/wordpress/wp-content/db.php" '$mounts + [{source: $source, target: $target, mode: "readonly"}]')
    fi

    printf '%s\n' "$mounts_json"
}

# Emit (to stdout) a PHP wrapper that, when executed inside the booted-WP
# sandbox via wordpress.run-php, requires the mounted smoke file and maps a
# thrown exception / fatal into a non-zero exit so the recipe records a failure.
# This prints the wrapper source; it does not run the smoke on the host.
homeboy_wordpress_smoke_wrapper() {
    local sandbox_smoke_path="$1"
    php -r '
        $smoke = $argv[1];
        echo "<?php\n";
        echo "\$smoke = " . var_export($smoke, true) . ";\n";
        echo "if (!file_exists(\$smoke)) { fwrite(STDERR, \"smoke file missing in sandbox: \" . \$smoke . \"\\n\"); exit(3); }\n";
        echo "register_shutdown_function(function () { \$e = error_get_last(); if (\$e && in_array(\$e[\"type\"], array(E_ERROR, E_PARSE, E_CORE_ERROR, E_COMPILE_ERROR), true)) { exit(1); } });\n";
        echo "try { require \$smoke; } catch (\\Throwable \$e) { fwrite(STDERR, \"smoke threw: \" . \$e->getMessage() . \"\\n\"); exit(1); }\n";
    ' "$sandbox_smoke_path"
}

run_one_smoke() {
    local smoke_file="$1"
    local rel_path="${smoke_file#"${PLUGIN_PATH}/"}"
    local sandbox_smoke_path="/wordpress/wp-content/plugins/${PLUGIN_SLUG}/${rel_path}"
    local wrapper_file recipe_file output_file artifacts_dir status

    wrapper_file="$(mktemp "${TMPDIR:-/tmp}/homeboy-wp-smoke-wrapper.XXXXXX")"
    recipe_file="$(mktemp "${TMPDIR:-/tmp}/homeboy-wp-smoke-recipe.XXXXXX")"
    output_file="$(mktemp "${TMPDIR:-/tmp}/homeboy-wp-smoke-output.XXXXXX")"
    artifacts_dir="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wp-smoke-artifacts.XXXXXX")"

    homeboy_wordpress_smoke_wrapper "$sandbox_smoke_path" > "$wrapper_file"

    jq -n \
        --arg wp "$WP_VERSION" \
        --argjson mounts "$RECIPE_MOUNTS" \
        --arg codeFile "$wrapper_file" \
        '{
            schema: "wp-codebox/workspace-recipe/v1",
            runtime: ({blueprint: {steps: []}} + (if $wp == "" then {} else {wp: $wp} end)),
            inputs: {mounts: $mounts},
            workflow: {steps: [{command: "wordpress.run-php", args: ["code-file=" + $codeFile]}]}
        }' > "$recipe_file"

    set +e
    "${WP_CODEBOX_COMMAND[@]}" recipe-run --recipe "$recipe_file" --artifacts "$artifacts_dir" --json > "$output_file" 2>/dev/null
    status=$?
    set -e

    # Surface the smoke's own stdout (the OK / failure text) for the operator.
    jq -r '(.executions // [])[-1].stdout // empty' "$output_file" 2>/dev/null || true
    if [ "$status" -eq 0 ] && ! jq -e '.success == true' "$output_file" >/dev/null 2>&1; then
        status=1
    fi
    if [ "$status" -ne 0 ]; then
        jq -r '(.executions // [])[-1].stderr // empty' "$output_file" 2>/dev/null >&2 || true
    fi

    rm -f "$wrapper_file" "$recipe_file" "$output_file"
    rm -rf "$artifacts_dir"
    return "$status"
}

echo "Running real-WordPress smoke tests..."
echo "  Component: ${PLUGIN_SLUG} (${PLUGIN_PATH})"
echo "  Backend: host-smoke-wp"

if [ ! -d "$TEST_DIR" ]; then
    echo ""
    echo "Skipping real-WordPress smoke tests: no tests directory found at ${TEST_DIR}"
    exit 0
fi

if [ -n "$TARGET_SMOKE_FILES" ]; then
    smoke_files=()
    while IFS= read -r smoke_file; do
        [ -z "$smoke_file" ] && continue
        if ! smoke_abs="$(homeboy_wordpress_host_smoke_abs "$smoke_file")"; then
            exit 2
        fi
        smoke_files+=("$smoke_abs")
    done <<< "$TARGET_SMOKE_FILES"
elif [ -n "$TARGET_SMOKE_FILE" ]; then
    if ! target_abs="$(homeboy_wordpress_host_smoke_abs "$TARGET_SMOKE_FILE")"; then
        exit 2
    fi
    smoke_files=("$target_abs")
else
    mapfile -t smoke_files < <(find "$TEST_DIR" -type f -name '*-smoke.php' | sort)
fi

if [ "${#smoke_files[@]}" -eq 0 ]; then
    echo ""
    echo "Skipping real-WordPress smoke tests: no files matched ${TEST_DIR}/**/*-smoke.php"
    exit 0
fi

WP_CODEBOX_BIN="$(homeboy_wordpress_resolve_wp_codebox_bin)" || exit 1
WP_CODEBOX_COMMAND=("$WP_CODEBOX_BIN")
case "$WP_CODEBOX_BIN" in
    *.js|*.cjs)
        WP_CODEBOX_COMMAND=(node "$WP_CODEBOX_BIN")
        ;;
esac
WP_VERSION="$(homeboy_wordpress_smoke_wp_version)"
RECIPE_MOUNTS="$(homeboy_wordpress_smoke_recipe_mounts)"

echo "  Files: ${#smoke_files[@]}"
echo "  WordPress: ${WP_VERSION:-default}"
echo ""

passed=0
for smoke_file in "${smoke_files[@]}"; do
    rel_path="${smoke_file#"${PLUGIN_PATH}/"}"
    echo "HOST_SMOKE_BEGIN:${rel_path}"
    if run_one_smoke "$smoke_file"; then
        echo "HOST_SMOKE_OK:${rel_path}"
        passed=$((passed + 1))
    else
        exit_code=$?
        echo "HOST_SMOKE_FAIL:${rel_path}:exit=${exit_code}"
        echo ""
        echo "Real-WordPress smoke test failed: ${rel_path}"
        exit "$exit_code"
    fi
done

echo ""
echo "HOST_SMOKE_SUMMARY:passed=${passed} failed=0"
echo "Real-WordPress smoke test run complete."
