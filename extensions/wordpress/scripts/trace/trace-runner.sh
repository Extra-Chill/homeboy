#!/usr/bin/env bash
set -euo pipefail

# Trace runner router for WordPress Homeboy extension.
#
# Project-owned scenarios live in the component under one of:
# - traces/<scenario>.trace.php
# - tests/traces/<scenario>.trace.php
# - scripts/trace/<scenario>.sh

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SHARED_LIB_DIR="${HOMEBOY_SHARED_LIB_DIR:-}"
if [ -z "$SHARED_LIB_DIR" ] && [ -n "${HOMEBOY_EXTENSION_PATH:-}" ] && [ -d "${HOMEBOY_EXTENSION_PATH}/../scripts/lib" ]; then
    SHARED_LIB_DIR="$(cd "${HOMEBOY_EXTENSION_PATH}/../scripts/lib" && pwd)"
fi
SHARED_LIB_DIR="${SHARED_LIB_DIR:-$(cd "${SCRIPT_DIR}/../../../scripts/lib" && pwd)}"
# shellcheck source=/dev/null
source "${SHARED_LIB_DIR}/runner-harness.sh"
homeboy_runner_harness_init --component-alias PLUGIN_PATH

WP_CODEBOX_PATHS_HELPER="${SCRIPT_DIR}/../lib/wp-codebox-paths.sh"
# shellcheck source=../lib/wp-codebox-paths.sh
source "$WP_CODEBOX_PATHS_HELPER"
DEPENDENCY_HELPER="${HOMEBOY_WORDPRESS_DEPENDENCY_HELPER:-${SCRIPT_DIR}/../lib/validation-dependencies.sh}"
# shellcheck source=../lib/validation-dependencies.sh
if [ -f "$DEPENDENCY_HELPER" ]; then
    source "$DEPENDENCY_HELPER"
fi

export HOMEBOY_COMPONENT_PATH="${HOMEBOY_COMPONENT_PATH:-$COMPONENT_PATH}"
export HOMEBOY_TRACE_COMPONENT_PATH="${HOMEBOY_TRACE_COMPONENT_PATH:-$HOMEBOY_COMPONENT_PATH}"
export HOMEBOY_COMPONENT_ID="${HOMEBOY_COMPONENT_ID:-$COMPONENT_ID}"
export HOMEBOY_PROJECT_PATH="${HOMEBOY_PROJECT_PATH:-$PROJECT_PATH}"

homeboy_wordpress_find_root() {
    if [ -n "${HOMEBOY_WORDPRESS_ROOT:-}" ] && [ -d "$HOMEBOY_WORDPRESS_ROOT" ]; then
        printf '%s\n' "$HOMEBOY_WORDPRESS_ROOT"
        return 0
    fi

    local cursor="${HOMEBOY_COMPONENT_PATH}"
    while [ "$cursor" != "/" ] && [ -n "$cursor" ]; do
        if [ -f "$cursor/wp-config.php" ] || [ -d "$cursor/wp-includes" ]; then
            printf '%s\n' "$cursor"
            return 0
        fi
        cursor="$(dirname "$cursor")"
    done

    if [ -d "/wordpress" ]; then
        printf '%s\n' "/wordpress"
        return 0
    fi

    return 1
}

homeboy_wordpress_export_context() {
    local wp_root=""
    if wp_root="$(homeboy_wordpress_find_root)"; then
        export HOMEBOY_WORDPRESS_ROOT="$wp_root"
    fi

    if [ -z "${HOMEBOY_WP_CLI:-}" ] && command -v wp >/dev/null 2>&1; then
        if [ -n "${HOMEBOY_WORDPRESS_ROOT:-}" ]; then
            export HOMEBOY_WP_CLI="wp --path=${HOMEBOY_WORDPRESS_ROOT}"
        else
            export HOMEBOY_WP_CLI="wp"
        fi
    fi
}

homeboy_wordpress_resolve_wp_codebox_bin() {
    homeboy_wp_codebox_resolve_bin "${HOMEBOY_SETTINGS_JSON:-}"
}

homeboy_wordpress_trace_wp_version() {
    local version=""
    local extracted=""

    if [ -n "${HOMEBOY_SETTINGS_JSON:-}" ] && [ "${HOMEBOY_SETTINGS_JSON}" != "{}" ]; then
        extracted=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -r '.wordpress_runtime_version // .wp_codebox_wordpress_version // empty' 2>/dev/null || true)
        [ -n "$extracted" ] && [ "$extracted" != "null" ] && version="$extracted"
    fi

    printf '%s\n' "$version"
}

homeboy_wordpress_trace_php_wrapper() {
    local runtime_scenario="$1"
    local runtime_results="$2"
    local runtime_artifacts="$3"
    local runtime_run_dir="$4"

    php -r '
        $env = array(
            "HOMEBOY_COMPONENT_PATH" => "/wordpress/wp-content/plugins/" . getenv("PLUGIN_SLUG"),
            "HOMEBOY_TRACE_COMPONENT_PATH" => "/wordpress/wp-content/plugins/" . getenv("PLUGIN_SLUG"),
            "HOMEBOY_PLUGIN_PATH" => "/wordpress/wp-content/plugins/" . getenv("PLUGIN_SLUG"),
            "HOMEBOY_COMPONENT_ID" => getenv("HOMEBOY_COMPONENT_ID") ?: basename(getenv("HOMEBOY_COMPONENT_PATH") ?: getcwd()),
            "HOMEBOY_PROJECT_PATH" => getenv("HOMEBOY_PROJECT_PATH") ?: getenv("HOMEBOY_COMPONENT_PATH"),
            "HOMEBOY_TRACE_SCENARIO" => getenv("HOMEBOY_TRACE_SCENARIO"),
            "HOMEBOY_TRACE_RESULTS_FILE" => $argv[2],
            "HOMEBOY_TRACE_ARTIFACT_DIR" => $argv[3],
            "HOMEBOY_RUN_DIR" => $argv[4],
            "HOMEBOY_WP_CLI" => "wp",
        );

        foreach ($env as $name => $value) {
            echo "putenv(" . var_export($name . "=" . $value, true) . ");\n";
        }

        echo "require " . var_export($argv[1], true) . ";\n";
    ' "$runtime_scenario" "$runtime_results" "$runtime_artifacts" "$runtime_run_dir"
}

homeboy_trace_run_php_scenario_wp_codebox() {
    local scenario_rel="$1"
    local stdout_file="$2"
    local stderr_file="$3"
    local codebox_bin wp_version plugin_slug run_mount_host runtime_run_dir runtime_results runtime_artifacts runtime_scenario wrapper_file recipe_file output_file codebox_artifacts_dir status mounts_json dep_path dep_slug dep_source

    codebox_bin="$(homeboy_wordpress_resolve_wp_codebox_bin)" || return 1
    wp_version="$(homeboy_wordpress_trace_wp_version)"
    plugin_slug="$(basename "$HOMEBOY_COMPONENT_PATH")"
    run_mount_host="$(homeboy_wp_codebox_resolve_mount_path "$HOMEBOY_RUN_DIR")"
    runtime_run_dir="/homeboy-trace-run"
    case "$HOMEBOY_TRACE_RESULTS_FILE" in
        "${HOMEBOY_RUN_DIR}"/*)
            runtime_results="${runtime_run_dir}/${HOMEBOY_TRACE_RESULTS_FILE#"${HOMEBOY_RUN_DIR}/"}"
            ;;
        *)
            echo "Error: HOMEBOY_TRACE_RESULTS_FILE must live under HOMEBOY_RUN_DIR for WP Codebox trace execution: ${HOMEBOY_TRACE_RESULTS_FILE}" >&2
            return 1
            ;;
    esac
    case "$HOMEBOY_TRACE_ARTIFACT_DIR" in
        "${HOMEBOY_RUN_DIR}"/*)
            runtime_artifacts="${runtime_run_dir}/${HOMEBOY_TRACE_ARTIFACT_DIR#"${HOMEBOY_RUN_DIR}/"}"
            ;;
        *)
            echo "Error: HOMEBOY_TRACE_ARTIFACT_DIR must live under HOMEBOY_RUN_DIR for WP Codebox trace execution: ${HOMEBOY_TRACE_ARTIFACT_DIR}" >&2
            return 1
            ;;
    esac
    runtime_scenario="/wordpress/wp-content/plugins/${plugin_slug}/${scenario_rel}"

    wrapper_file="$(mktemp "${TMPDIR:-/tmp}/homeboy-wp-codebox-trace-wrapper.XXXXXX")"
    recipe_file="$(mktemp "${TMPDIR:-/tmp}/homeboy-wp-codebox-trace-recipe.XXXXXX")"
    output_file="$(mktemp "${TMPDIR:-/tmp}/homeboy-wp-codebox-trace-output.XXXXXX")"
    codebox_artifacts_dir="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wp-codebox-trace-artifacts.XXXXXX")"
    PLUGIN_SLUG="$plugin_slug" homeboy_wordpress_trace_php_wrapper "$runtime_scenario" "$runtime_results" "$runtime_artifacts" "$runtime_run_dir" > "$wrapper_file"

    mounts_json=$(jq -nc \
        --arg componentSource "$(homeboy_wp_codebox_resolve_mount_path "$HOMEBOY_COMPONENT_PATH")" \
        --arg componentTarget "/wordpress/wp-content/plugins/${plugin_slug}" \
        --arg runSource "$run_mount_host" \
        --arg runTarget "$runtime_run_dir" \
        '[
            {source: $componentSource, target: $componentTarget, mode: "readwrite"},
            {source: $runSource, target: $runTarget, mode: "readwrite"}
        ]')

    if [ -n "${HOMEBOY_WORDPRESS_DEPENDENCY_PATHS:-}" ]; then
        while IFS= read -r dep_path; do
            [ -n "$dep_path" ] || continue
            [ -d "$dep_path" ] || continue
            dep_slug="$(homeboy_get_validation_dependency_slug "$dep_path" || basename "$dep_path")"
            dep_source="$(homeboy_wp_codebox_resolve_mount_path "$dep_path")"
            mounts_json=$(jq -nc \
                --argjson mounts "$mounts_json" \
                --arg source "$dep_source" \
                --arg target "/wordpress/wp-content/plugins/${dep_slug}" \
                '$mounts + [{source: $source, target: $target, mode: "readwrite"}]')
        done <<< "$HOMEBOY_WORDPRESS_DEPENDENCY_PATHS"
    fi

    jq -n \
        --arg wp "$wp_version" \
        --argjson mounts "$mounts_json" \
        --arg codeFile "$wrapper_file" \
        '{
            schema: "wp-codebox/workspace-recipe/v1",
            runtime: ({blueprint: {steps: []}} + (if $wp == "" then {} else {wp: $wp} end)),
            inputs: {mounts: $mounts},
            workflow: {steps: [{command: "wordpress.run-php", args: ["code-file=" + $codeFile]}]}
        }' > "$recipe_file"

    set +e
    homeboy_wp_codebox_run_recipe "$recipe_file" "$codebox_artifacts_dir" "$output_file" "$stderr_file" "$codebox_bin"
    status=$?
    set -e

    homeboy_wp_codebox_recipe_last_stdout "$output_file" > "$stdout_file" || true
    if [ ! -s "$stderr_file" ]; then
        homeboy_wp_codebox_recipe_last_stderr "$output_file" > "$stderr_file" || true
    fi

    if [ "$status" -eq 0 ] && ! homeboy_wp_codebox_recipe_succeeded "$output_file"; then
        status=1
    fi

    if [ "$status" -ne 0 ]; then
        cat "$output_file" >> "$stderr_file"
    fi

    rm -f "$wrapper_file" "$recipe_file" "$output_file"
    rm -rf "$codebox_artifacts_dir"
    return "$status"
}

homeboy_trace_emit_scenario() {
    local id="$1"
    local source="$2"

    printf '%s\t%s\n' "$id" "$source"
}

homeboy_trace_discover() {
    local seen="|"
    local file id rel

    shopt -s nullglob
    for file in \
        "${HOMEBOY_COMPONENT_PATH}"/traces/*.trace.php \
        "${HOMEBOY_COMPONENT_PATH}"/tests/traces/*.trace.php \
        "${HOMEBOY_COMPONENT_PATH}"/scripts/trace/*.sh; do
        rel="${file#"${HOMEBOY_COMPONENT_PATH}/"}"
        case "$rel" in
            traces/*.trace.php)
                id="${rel#traces/}"
                id="${id%.trace.php}"
                ;;
            tests/traces/*.trace.php)
                id="${rel#tests/traces/}"
                id="${id%.trace.php}"
                ;;
            scripts/trace/*.sh)
                id="${rel#scripts/trace/}"
                id="${id%.sh}"
                ;;
            *)
                continue
                ;;
        esac

        case "$seen" in
            *"|${id}|"*) continue ;;
        esac
        seen="${seen}${id}|"

        homeboy_trace_emit_scenario "$id" "$rel"
    done
    shopt -u nullglob
}

homeboy_trace_list_json() {
    php -r '
        $scenarios = array();
        foreach (file("php://stdin", FILE_IGNORE_NEW_LINES | FILE_SKIP_EMPTY_LINES) as $line) {
            $parts = explode("\t", $line, 2);
            if (count($parts) !== 2) {
                continue;
            }
            $scenarios[] = array(
                "id" => $parts[0],
                "source" => $parts[1],
            );
        }
        $data = array(
            "component_id" => getenv("HOMEBOY_COMPONENT_ID") ?: basename(getenv("HOMEBOY_COMPONENT_PATH") ?: getcwd()),
            "scenario_id" => "__list__",
            "status" => "pass",
            "scenarios" => $scenarios,
            "timeline" => array(),
            "assertions" => array(),
            "artifacts" => array(),
        );
        echo json_encode($data, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES) . PHP_EOL;
    '
}

homeboy_trace_resolve_scenario() {
    local scenario="$1"
    local candidate

    for candidate in \
        "traces/${scenario}.trace.php" \
        "tests/traces/${scenario}.trace.php" \
        "scripts/trace/${scenario}.sh"; do
        if [ -f "${HOMEBOY_COMPONENT_PATH}/${candidate}" ]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done

    return 1
}

homeboy_trace_write_failure() {
    local status="$1"
    local summary="$2"

    if [ -z "${HOMEBOY_TRACE_RESULTS_FILE:-}" ]; then
        return 0
    fi

    php -r '
        $path = getenv("HOMEBOY_TRACE_RESULTS_FILE");
        $data = array(
            "component_id" => getenv("HOMEBOY_COMPONENT_ID") ?: basename(getenv("HOMEBOY_COMPONENT_PATH") ?: getcwd()),
            "scenario_id" => getenv("HOMEBOY_TRACE_SCENARIO") ?: "unknown",
            "status" => $argv[1],
            "summary" => $argv[2],
            "timeline" => array(),
            "assertions" => array(
                array(
                    "id" => "runner",
                    "status" => $argv[1],
                    "message" => $argv[2],
                ),
            ),
            "artifacts" => array(),
        );
        file_put_contents($path, json_encode($data, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES) . PHP_EOL);
    ' "$status" "$summary"
}

if [ "${HOMEBOY_TRACE_LIST_ONLY:-}" = "1" ]; then
    homeboy_trace_discover | homeboy_trace_list_json
    exit 0
fi

if [ -z "${HOMEBOY_TRACE_SCENARIO:-}" ]; then
    echo "ERROR: HOMEBOY_TRACE_SCENARIO is required" >&2
    exit 2
fi

if ! scenario_rel="$(homeboy_trace_resolve_scenario "$HOMEBOY_TRACE_SCENARIO")"; then
    echo "ERROR: WordPress trace scenario not found: ${HOMEBOY_TRACE_SCENARIO}" >&2
    echo "Searched traces/*.trace.php, tests/traces/*.trace.php, scripts/trace/*.sh under ${HOMEBOY_COMPONENT_PATH}" >&2
    exit 2
fi

export HOMEBOY_TRACE_ARTIFACT_DIR="${HOMEBOY_TRACE_ARTIFACT_DIR:-${HOMEBOY_RUN_DIR:-${HOMEBOY_COMPONENT_PATH}/.homeboy}/trace-artifacts}"
export HOMEBOY_RUN_DIR="${HOMEBOY_RUN_DIR:-$(dirname "$HOMEBOY_TRACE_ARTIFACT_DIR")}"
export HOMEBOY_TRACE_RESULTS_FILE="${HOMEBOY_TRACE_RESULTS_FILE:-${HOMEBOY_RUN_DIR}/trace-${HOMEBOY_TRACE_SCENARIO}.json}"

mkdir -p "$HOMEBOY_TRACE_ARTIFACT_DIR" "$(dirname "$HOMEBOY_TRACE_RESULTS_FILE")"
homeboy_wordpress_export_context

if type homeboy_preflight_declared_validation_dependency_paths &>/dev/null; then
    if ! homeboy_preflight_declared_validation_dependency_paths "$HOMEBOY_TRACE_ARTIFACT_DIR" "trace"; then
        homeboy_trace_write_failure "fail" "WordPress dependency plugin preflight failed before WP Codebox dispatch"
        exit 1
    fi
fi
if type homeboy_export_validation_dependency_paths &>/dev/null; then
    homeboy_export_validation_dependency_paths "$HOMEBOY_COMPONENT_PATH"
fi
if [ -n "${HOMEBOY_WORDPRESS_DEPENDENCY_PATHS:-}" ] && type homeboy_prepare_validation_dependency_paths_for_wp_codebox_runtime &>/dev/null; then
    if ! HOMEBOY_WORDPRESS_DEPENDENCY_PATHS=$(homeboy_prepare_validation_dependency_paths_for_wp_codebox_runtime "$HOMEBOY_WORDPRESS_DEPENDENCY_PATHS" "$HOMEBOY_TRACE_ARTIFACT_DIR" "trace"); then
        homeboy_trace_write_failure "fail" "WordPress dependency plugin preparation failed before WP Codebox dispatch"
        exit 1
    fi
    export HOMEBOY_WORDPRESS_DEPENDENCY_PATHS
fi
if [ -n "${HOMEBOY_WORDPRESS_DEPENDENCY_PATHS:-}" ] && type homeboy_preflight_wordpress_dependency_plugins &>/dev/null; then
    if ! homeboy_preflight_wordpress_dependency_plugins "$HOMEBOY_WORDPRESS_DEPENDENCY_PATHS" "$HOMEBOY_TRACE_ARTIFACT_DIR" "trace"; then
        homeboy_trace_write_failure "fail" "WordPress dependency plugin preflight failed before WP Codebox dispatch"
        exit 1
    fi
fi

stdout_file="${HOMEBOY_TRACE_ARTIFACT_DIR}/${HOMEBOY_TRACE_SCENARIO}.stdout.txt"
stderr_file="${HOMEBOY_TRACE_ARTIFACT_DIR}/${HOMEBOY_TRACE_SCENARIO}.stderr.txt"
exit_file="${HOMEBOY_TRACE_ARTIFACT_DIR}/${HOMEBOY_TRACE_SCENARIO}.exit-code.txt"

scenario_abs="${HOMEBOY_COMPONENT_PATH}/${scenario_rel}"
set +e
case "$scenario_rel" in
    *.php)
        homeboy_trace_run_php_scenario_wp_codebox "$scenario_rel" "$stdout_file" "$stderr_file"
        status=$?
        ;;
    *.sh)
        bash "$scenario_abs" >"$stdout_file" 2>"$stderr_file"
        status=$?
        ;;
    *)
        echo "ERROR: unsupported WordPress trace scenario file: ${scenario_rel}" >&2
        status=2
        ;;
esac
set -e

printf '%s\n' "$status" >"$exit_file"

if [ "$status" -ne 0 ]; then
    homeboy_trace_write_failure "fail" "WordPress trace scenario exited with status ${status}"
    exit "$status"
fi

if [ ! -s "$HOMEBOY_TRACE_RESULTS_FILE" ]; then
    homeboy_trace_write_failure "fail" "WordPress trace scenario completed without writing HOMEBOY_TRACE_RESULTS_FILE"
    exit 1
fi

exit 0
