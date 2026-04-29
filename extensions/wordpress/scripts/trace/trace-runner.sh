#!/usr/bin/env bash
set -euo pipefail

# Trace runner router for WordPress Homeboy extension.
#
# Project-owned scenarios live in the component under one of:
# - traces/<scenario>.trace.php
# - tests/traces/<scenario>.trace.php
# - scripts/trace/<scenario>.sh

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${SCRIPT_DIR}/../lib/resolve-context.sh}"
# shellcheck source=/dev/null
source "$RESOLVE_CONTEXT_HELPER"
homeboy_resolve_context --component-alias PLUGIN_PATH

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

stdout_file="${HOMEBOY_TRACE_ARTIFACT_DIR}/${HOMEBOY_TRACE_SCENARIO}.stdout.txt"
stderr_file="${HOMEBOY_TRACE_ARTIFACT_DIR}/${HOMEBOY_TRACE_SCENARIO}.stderr.txt"
exit_file="${HOMEBOY_TRACE_ARTIFACT_DIR}/${HOMEBOY_TRACE_SCENARIO}.exit-code.txt"

scenario_abs="${HOMEBOY_COMPONENT_PATH}/${scenario_rel}"
set +e
case "$scenario_rel" in
    *.php)
        php "$scenario_abs" >"$stdout_file" 2>"$stderr_file"
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
