#!/usr/bin/env bash
set -euo pipefail

# WP Codebox-backed WordPress PHPUnit runner.
#
# This translates the component/dependency/drop-in/file-mount/config/env/version
# contract into wp-codebox run arguments for the default WordPress PHPUnit path.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:?HOMEBOY_RUNTIME_RESOLVE_CONTEXT is required}"
RUNNER_STEPS_HELPER="${HOMEBOY_RUNTIME_RUNNER_STEPS:?HOMEBOY_RUNTIME_RUNNER_STEPS is required}"
FAILURE_TRAP_HELPER="${HOMEBOY_RUNTIME_FAILURE_TRAP:-}"
WRITE_TEST_RESULTS_HELPER="${HOMEBOY_RUNTIME_WRITE_TEST_RESULTS:-}"
DEPENDENCY_HELPER="${HOMEBOY_WORDPRESS_DEPENDENCY_HELPER:-${SCRIPT_DIR}/../lib/validation-dependencies.sh}"
PHP_PREFLIGHT_HELPER="${SCRIPT_DIR}/../lib/php-preflight.sh"
WP_CODEBOX_PATHS_HELPER="${SCRIPT_DIR}/../lib/wp-codebox-paths.sh"
PHPUNIT_RECIPE_BUILDER="${HOMEBOY_WP_CODEBOX_PHPUNIT_RECIPE_BUILDER:-${SCRIPT_DIR}/build-wp-codebox-phpunit-recipe.mjs}"

# shellcheck source=/dev/null
source "$RESOLVE_CONTEXT_HELPER"
homeboy_resolve_context --component-alias PLUGIN_PATH
# shellcheck source=/dev/null
if [ -f "$RUNNER_STEPS_HELPER" ]; then
    source "$RUNNER_STEPS_HELPER"
fi
# shellcheck source=../lib/validation-dependencies.sh
if [ -f "$DEPENDENCY_HELPER" ]; then
    source "$DEPENDENCY_HELPER"
fi
# shellcheck source=../lib/php-preflight.sh
if [ -f "$PHP_PREFLIGHT_HELPER" ]; then
    source "$PHP_PREFLIGHT_HELPER"
fi
# shellcheck source=../lib/wp-codebox-paths.sh
source "$WP_CODEBOX_PATHS_HELPER"
# shellcheck source=/dev/null
if [ -n "$FAILURE_TRAP_HELPER" ] && [ -f "$FAILURE_TRAP_HELPER" ]; then
    source "$FAILURE_TRAP_HELPER"
    homeboy_init_failure_trap
else
    FAILED_STEP=""
    FAILURE_OUTPUT=""
    FAILURE_REPLAY_MODE="full"
fi
# shellcheck source=/dev/null
if [ -n "$WRITE_TEST_RESULTS_HELPER" ] && [ -f "$WRITE_TEST_RESULTS_HELPER" ]; then
    source "$WRITE_TEST_RESULTS_HELPER"
fi

if [ -n "${COMPONENT_ID:-}" ]; then
    PLUGIN_SLUG="$COMPONENT_ID"
else
    PLUGIN_SLUG="$(basename "$PLUGIN_PATH")"
fi

settings_json="${HOMEBOY_SETTINGS_JSON:-}"
[ -n "$settings_json" ] || settings_json="{}"

WP_CODEBOX_SOURCE_ROOT=""
WP_CODEBOX_SOURCE_SUBPATH=""
WP_CODEBOX_PLUGIN_SOURCE_PATH="$PLUGIN_PATH"
if [ "$settings_json" != "{}" ]; then
    WP_CODEBOX_SOURCE_ROOT=$(printf '%s' "$settings_json" | jq -r '.wp_codebox_source_root // empty' 2>/dev/null || true)
    WP_CODEBOX_SOURCE_SUBPATH=$(printf '%s' "$settings_json" | jq -r '.wp_codebox_source_subpath // empty' 2>/dev/null || true)
fi
if [ -z "$WP_CODEBOX_SOURCE_ROOT" ] && [ -n "${HOMEBOY_SETTINGS_WP_CODEBOX_SOURCE_ROOT:-}" ]; then
    WP_CODEBOX_SOURCE_ROOT="$HOMEBOY_SETTINGS_WP_CODEBOX_SOURCE_ROOT"
fi
if [ -z "$WP_CODEBOX_SOURCE_SUBPATH" ] && [ -n "${HOMEBOY_SETTINGS_WP_CODEBOX_SOURCE_SUBPATH:-}" ]; then
    WP_CODEBOX_SOURCE_SUBPATH="$HOMEBOY_SETTINGS_WP_CODEBOX_SOURCE_SUBPATH"
fi
if [ -n "$WP_CODEBOX_SOURCE_ROOT" ]; then
    if [[ "$WP_CODEBOX_SOURCE_ROOT" != /* ]] || [ ! -d "$WP_CODEBOX_SOURCE_ROOT" ]; then
        echo "Error: wp_codebox_source_root must be an absolute existing directory." >&2
        FAILED_STEP="WP Codebox source root setup"
        exit 1
    fi
    if [ -z "$WP_CODEBOX_SOURCE_SUBPATH" ]; then
        if [[ "$PLUGIN_PATH" = "$WP_CODEBOX_SOURCE_ROOT"/* ]]; then
            WP_CODEBOX_SOURCE_SUBPATH="${PLUGIN_PATH#"$WP_CODEBOX_SOURCE_ROOT/"}"
        else
            echo "Error: wp_codebox_source_subpath is required when wp_codebox_source_root is not an ancestor of HOMEBOY_COMPONENT_PATH." >&2
            FAILED_STEP="WP Codebox source root setup"
            exit 1
        fi
    fi
    if [[ "$WP_CODEBOX_SOURCE_SUBPATH" = /* ]] || [[ "$WP_CODEBOX_SOURCE_SUBPATH" == *..* ]]; then
        echo "Error: wp_codebox_source_subpath must be a relative path under wp_codebox_source_root." >&2
        FAILED_STEP="WP Codebox source root setup"
        exit 1
    fi
    WP_CODEBOX_PLUGIN_SOURCE_PATH="${WP_CODEBOX_SOURCE_ROOT%/}/${WP_CODEBOX_SOURCE_SUBPATH}"
    if [ ! -d "$WP_CODEBOX_PLUGIN_SOURCE_PATH" ]; then
        echo "Error: wp_codebox_source_root/wp_codebox_source_subpath does not exist: $WP_CODEBOX_PLUGIN_SOURCE_PATH" >&2
        FAILED_STEP="WP Codebox source root setup"
        exit 1
    fi
    PLUGIN_PATH="$WP_CODEBOX_PLUGIN_SOURCE_PATH"
fi

detect_network_plugin_header() {
    local main_file
    for main_file in "${PLUGIN_PATH}"/*.php; do
        [ -f "$main_file" ] || continue
        if grep -q '^[[:space:]]*Plugin Name:' "$main_file" && grep -qi '^[[:space:]]*Network:[[:space:]]*true[[:space:]]*$' "$main_file"; then
            return 0
        fi
    done
    return 1
}

component_has_composer_test_script() {
    [ -f "${PLUGIN_PATH}/composer.json" ] || return 1

    php -r '
        $composer = json_decode(file_get_contents($argv[1]), true);
        exit(is_array($composer) && isset($composer["scripts"]["test"]) ? 0 : 1);
    ' "${PLUGIN_PATH}/composer.json" 2>/dev/null
}

component_npm_test_script() {
    [ -f "${PLUGIN_PATH}/package.json" ] || return 1

    NPM_TEST_SCRIPT="$(php -r '
        $settings = json_decode(getenv("HOMEBOY_SETTINGS_JSON") ?: "{}", true);
        $package = json_decode(file_get_contents($argv[1]), true);
        if (!is_array($settings)) {
            $settings = [];
        }
        if (!is_array($package) || !isset($package["scripts"]) || !is_array($package["scripts"])) {
            exit(1);
        }
        $script = $settings["npm_test_script"] ?? $settings["node_test_script"] ?? $settings["test_npm_script"] ?? null;
        if ($script === null && isset($package["scripts"]["test"])) {
            $script = "test";
        }
        if (!is_string($script) || $script === "" || !isset($package["scripts"][$script])) {
            exit(1);
        }
        echo $script;
    ' "${PLUGIN_PATH}/package.json" 2>/dev/null)" || return 1

    [ -n "$NPM_TEST_SCRIPT" ]
}

run_composer_test_script() {
    echo ""
    echo "Running Composer test script..."
    echo "  Plugin: ${PLUGIN_SLUG} (${PLUGIN_PATH})"
    echo "  Backend: composer-script"

    if ! command -v composer >/dev/null 2>&1; then
        echo "ERROR: composer.json declares scripts.test, but composer is not available on PATH." >&2
        FAILED_STEP="Composer test script setup"
        return 1
    fi

    if [ "${#PASSTHROUGH_ARGS[@]}" -gt 0 ]; then
        ( cd "${PLUGIN_PATH}" && composer test -- "${PASSTHROUGH_ARGS[@]}" )
    else
        ( cd "${PLUGIN_PATH}" && composer test )
    fi
}

run_npm_test_script() {
    echo ""
    echo "Running npm test script..."
    echo "  Plugin: ${PLUGIN_SLUG} (${PLUGIN_PATH})"
    echo "  Backend: npm-script"
    echo "  Script: ${NPM_TEST_SCRIPT}"

    if ! command -v npm >/dev/null 2>&1; then
        echo "ERROR: package.json declares scripts.${NPM_TEST_SCRIPT}, but npm is not available on PATH." >&2
        FAILED_STEP="npm test script setup"
        return 1
    fi

    if [ "${#PASSTHROUGH_ARGS[@]}" -gt 0 ]; then
        ( cd "${PLUGIN_PATH}" && npm run "$NPM_TEST_SCRIPT" -- "${PASSTHROUGH_ARGS[@]}" )
    else
        ( cd "${PLUGIN_PATH}" && npm run "$NPM_TEST_SCRIPT" )
    fi
}

write_phpunit_discovery_result() {
    local status="$1"
    local partial="$2"
    local message="$3"

    if ! type homeboy_write_test_results >/dev/null 2>&1; then
        return 0
    fi

    if [ "$status" = "failed" ]; then
        homeboy_write_test_results 1 0 1 0 "$partial"
    else
        homeboy_write_test_results 0 0 0 0 "$partial"
    fi

    if [ -n "${HOMEBOY_TEST_RESULTS_FILE:-}" ] && [ -f "$HOMEBOY_TEST_RESULTS_FILE" ]; then
        php -r '
            $path = $argv[1];
            $status = $argv[2];
            $message = $argv[3];
            $data = json_decode(file_get_contents($path), true);
            if (!is_array($data)) {
                $data = [];
            }
            $data["status"] = $status;
            if ($message !== "") {
                $data["message"] = $message;
            }
            file_put_contents($path, json_encode($data, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES) . PHP_EOL);
        ' "$HOMEBOY_TEST_RESULTS_FILE" "$status" "$message"
    fi
}

SELECTED_TEST_FILE="${HOMEBOY_WORDPRESS_PHPUNIT_TEST_FILE:-}"
PASSTHROUGH_ARGS=()
while [ "$#" -gt 0 ]; do
    case "$1" in
        --filter)
            PASSTHROUGH_ARGS+=("$1")
            shift
            if [ "$#" -eq 0 ] || [ -z "${1:-}" ]; then
                echo "ERROR: --filter requires a value" >&2
                exit 2
            fi
            PASSTHROUGH_ARGS+=("$1")
            ;;
        --file)
            shift
            if [ "$#" -eq 0 ] || [ -z "${1:-}" ]; then
                echo "ERROR: --file requires a path" >&2
                exit 2
            fi
            SELECTED_TEST_FILE="$1"
            ;;
        --file=*)
            SELECTED_TEST_FILE="${1#--file=}"
            ;;
        *)
            if [ -z "$SELECTED_TEST_FILE" ]; then
                if [ "${1#/}" != "$1" ]; then
                    candidate_test_file="$1"
                else
                    candidate_test_file="${PLUGIN_PATH}/${1}"
                fi

                if [ -f "$candidate_test_file" ]; then
                    SELECTED_TEST_FILE="$1"
                    shift
                    continue
                fi
            fi

            PASSTHROUGH_ARGS+=("$1")
            ;;
    esac
    shift
done

WP_CODEBOX_BIN="$(homeboy_wp_codebox_resolve_bin "${HOMEBOY_SETTINGS_JSON:-}")" || {
    FAILED_STEP="WP Codebox CLI setup"
    exit 1
}

WP_CODEBOX_CORE_MODULE="${HOMEBOY_SETTINGS_WP_CODEBOX_CORE_MODULE:-}"
if [ -z "$WP_CODEBOX_CORE_MODULE" ] && [ "$settings_json" != "{}" ]; then
    WP_CODEBOX_CORE_MODULE=$(printf '%s' "$settings_json" | jq -r '.wp_codebox_core_module // empty' 2>/dev/null || true)
fi
if [ -z "$WP_CODEBOX_CORE_MODULE" ]; then
    WP_CODEBOX_CORE_MODULE="${HOMEBOY_WP_CODEBOX_CORE_MODULE:-}"
fi
if [ -n "$WP_CODEBOX_CORE_MODULE" ]; then
    export HOMEBOY_WP_CODEBOX_CORE_MODULE="$WP_CODEBOX_CORE_MODULE"
fi

# Guard against a mis-resolved component path mounting the wrong directory.
#
# The recipe mounts PLUGIN_PATH wholesale into
# /wordpress/wp-content/plugins/<slug>, and the Playground runtime detects the
# plugin main file by globbing "*.php" for a "Plugin Name:" header and taking
# the first alphabetical match. If PLUGIN_PATH resolves to a shared scratch
# directory (e.g. a junk-drawer /tmp populated with stray debug .php files), an
# unrelated file like "an_main.php" wins detection and the wrong plugin is
# mounted under this component's slug — producing a confusing load_component
# fatal that looks like the component under test is broken when it is not.
#
# When the component identity is known, require PLUGIN_PATH to actually contain
# a WordPress plugin main file whose slug matches the component, and refuse to
# mount obvious shared scratch roots. Fail loud with an actionable message
# instead of silently mounting a junk directory.
guard_component_path() {
    # Only enforce when we know which component we are testing.
    [ -n "${COMPONENT_ID:-}" ] || return 0

    local scratch_tmpdir="${TMPDIR:-}"
    scratch_tmpdir="${scratch_tmpdir%/}"
    case "$PLUGIN_PATH" in
        /tmp | /tmp/ | /var/tmp | /var/tmp/ | ${scratch_tmpdir:+"$scratch_tmpdir"})
            echo "ERROR: Refusing to run tests against a shared temporary directory as the component source." >&2
            echo "  Component: ${COMPONENT_ID}" >&2
            echo "  Resolved path: ${PLUGIN_PATH}" >&2
            echo "  This usually means '--path .' was run from (or resolved to) a scratch directory." >&2
            echo "  Pass an explicit path to the component checkout, e.g. --path /abs/path/to/${COMPONENT_ID}" >&2
            return 1
            ;;
    esac

    # Collect plugin main files (a *.php in the root carrying a "Plugin Name:"
    # header) and check whether any matches this component's slug.
    #
    # Deliberately conservative to avoid false positives against legitimate
    # fixture layouts that ship a tests/ directory without a plugin header (those
    # route to host-smoke or composer-script backends and never reach the
    # wordpress.phpunit mount): only REJECT when a foreign plugin header is the
    # ONLY thing present and none matches the slug. If no plugin header exists at
    # all, defer to the downstream tests/ + composer-script handling.
    local found_main=""
    local slug_match=""
    local candidate
    for candidate in "${PLUGIN_PATH}"/*.php; do
        [ -f "$candidate" ] || continue
        grep -q '^[[:space:]]*\*\?[[:space:]]*Plugin Name:' "$candidate" || continue
        found_main="$candidate"
        if [ "$(basename "$candidate" .php)" = "${PLUGIN_SLUG}" ]; then
            slug_match="$candidate"
            break
        fi
    done

    if [ -n "$found_main" ] && [ -z "$slug_match" ]; then
        echo "ERROR: The resolved component path contains a WordPress plugin, but none matches the expected slug '${PLUGIN_SLUG}'." >&2
        echo "  Component: ${COMPONENT_ID}" >&2
        echo "  Resolved path: ${PLUGIN_PATH}" >&2
        echo "  Detected plugin main file instead: ${found_main}" >&2
        echo "  This typically means '--path' resolved to a directory that is not this component's checkout" >&2
        echo "  (for example a shared scratch directory holding a stray plugin file)." >&2
        echo "  Refusing to mount a mismatched directory under '${PLUGIN_SLUG}'. Pass an explicit --path to the checkout." >&2
        return 1
    fi

    return 0
}

if ! guard_component_path; then
    FAILED_STEP="Component path validation"
    write_phpunit_discovery_result failed "component-path-mismatch" "Resolved component path does not contain the expected plugin; refused to mount a mismatched or scratch directory."
    exit 1
fi

TEST_DIR="${PLUGIN_PATH}/tests"
if [ ! -d "$TEST_DIR" ]; then
    if component_has_composer_test_script; then
        run_composer_test_script
        exit $?
    fi

    if component_npm_test_script; then
        run_npm_test_script
        exit $?
    fi

    echo ""
    echo "Warning: No tests directory found at ${TEST_DIR}"
    echo "  Skipping PHPUnit tests."
    echo ""
    exit 0
fi

if type homeboy_php_preflight &>/dev/null; then
    homeboy_php_preflight "$PLUGIN_PATH"
fi

WP_TEST_SMELLS="${EXTENSION_PATH}/scripts/audit/wp-test-smells.py"
if [ -f "$WP_TEST_SMELLS" ]; then
    python3 "$WP_TEST_SMELLS" "$PLUGIN_PATH"
fi

compile_phpunit_bootstrap_files() {
    local settings_json="${HOMEBOY_SETTINGS_JSON:-}"
    [ -n "$settings_json" ] || settings_json="{}"
    local entries_json
    entries_json=$(printf '%s' "$settings_json" | jq -c '
        .wp_codebox_bootstrap_files // []
        | if type == "array" then . elif type == "string" then [.] else [] end
    ' 2>/dev/null || echo '[]')

    WP_CODEBOX_BOOTSTRAP_FILES_JSON="[]"
    if ! printf '%s' "$entries_json" | jq -e 'type == "array" and length > 0' >/dev/null 2>&1; then
        return 0
    fi

    local index=0
    while IFS= read -r bootstrap_ref; do
        [ -n "$bootstrap_ref" ] || continue
        if [[ "$bootstrap_ref" == *..* ]]; then
            echo "Error: wp_codebox_bootstrap_files[$index] must stay under component root: $bootstrap_ref" >&2
            FAILED_STEP="WP Codebox bootstrap file setup"
            exit 1
        fi

        local bootstrap_host
        bootstrap_host=$(homeboy_wp_codebox_resolve_host_path "$PLUGIN_PATH" "$bootstrap_ref")
        if [[ "$bootstrap_host" = /* && "$bootstrap_host" != "$PLUGIN_PATH"/* ]]; then
            echo "Error: wp_codebox_bootstrap_files[$index] must stay under component root: $bootstrap_ref" >&2
            FAILED_STEP="WP Codebox bootstrap file setup"
            exit 1
        fi
        if [ ! -f "$bootstrap_host" ]; then
            index=$((index + 1))
            continue
        fi

        local bootstrap_rel
        bootstrap_rel=$(homeboy_wp_codebox_component_relative_path "$bootstrap_host")
        WP_CODEBOX_BOOTSTRAP_FILES_JSON=$(jq -nc --argjson files "$WP_CODEBOX_BOOTSTRAP_FILES_JSON" --arg file "$bootstrap_rel" '$files + [$file]')
        index=$((index + 1))
    done < <(printf '%s' "$entries_json" | jq -r '.[] | select(type == "string" and . != "")')
}

compile_phpunit_bootstrap_files

compile_phpunit_prepare_steps() {
    local settings_json="${HOMEBOY_SETTINGS_JSON:-}"
    [ -n "$settings_json" ] || settings_json="{}"
    local steps_json
    steps_json=$(printf '%s' "$settings_json" | jq -c '
        .wp_codebox_prepare_steps // []
        | if type == "array" then . else [] end
    ' 2>/dev/null || echo '[]')

    WP_CODEBOX_PREPARE_STEPS_JSON="[]"
    if ! printf '%s' "$steps_json" | jq -e 'type == "array" and length > 0' >/dev/null 2>&1; then
        return 0
    fi

    if ! printf '%s' "$steps_json" | jq -e '
        all(.[];
            type == "object"
            and (.command | type == "string" and . != "")
            and ((.args // []) | type == "array" and all(.[]; type == "string"))
            and ((.cwd // "") | type == "string")
        )
    ' >/dev/null 2>&1; then
        echo "Error: wp_codebox_prepare_steps entries require command plus optional string args and cwd." >&2
        FAILED_STEP="WP Codebox PHPUnit prepare setup"
        exit 1
    fi

    WP_CODEBOX_PREPARE_STEPS_JSON="$steps_json"
}

apply_phpunit_component_prepare_profile() {
    case "$PLUGIN_SLUG" in
        woocommerce)
            if [ -f "${PLUGIN_PATH}/bin/generate-feature-config.php" ]; then
                WP_CODEBOX_PREPARE_STEPS_JSON=$(jq -nc \
                    --argjson steps "$WP_CODEBOX_PREPARE_STEPS_JSON" \
                    '$steps + [{command: "php", args: ["bin/generate-feature-config.php"]}]')
            fi
            ;;
    esac
}

component_needs_composer_autoload_prepare() {
    [ -f "${PLUGIN_PATH}/composer.json" ] || return 1

    local settings_json="${HOMEBOY_SETTINGS_JSON:-}"
    [ -n "$settings_json" ] || settings_json="{}"
    local mode
    mode=$(printf '%s' "$settings_json" | jq -r '.wp_codebox_composer_prepare // "auto"' 2>/dev/null || printf 'auto')

    case "$mode" in
        0|false|off|skip|disabled)
            return 1
            ;;
        1|true|on|enabled)
            return 0
            ;;
        auto)
            [ ! -f "${PLUGIN_PATH}/vendor/autoload.php" ] && return 0
            php -r '
                $composer = json_decode(file_get_contents($argv[1]), true);
                $files = $composer["autoload-dev"]["files"] ?? [];
                exit(is_array($composer) && is_array($files) && count($files) > 0 ? 0 : 1);
            ' "${PLUGIN_PATH}/composer.json" 2>/dev/null
            ;;
        *)
            echo "Error: wp_codebox_composer_prepare must be auto, true, or false (got '$mode')." >&2
            FAILED_STEP="WP Codebox Composer prepare setup"
            exit 1
            ;;
    esac
}

append_phpunit_component_composer_prepare_step() {
    component_needs_composer_autoload_prepare || return 0

    WP_CODEBOX_PREPARE_STEPS_JSON=$(jq -nc \
        --argjson steps "$WP_CODEBOX_PREPARE_STEPS_JSON" \
        '$steps + [{
            command: "composer",
            args: ["install", "--no-dev", "--no-interaction", "--no-progress", "--prefer-dist"]
        }]')
}

phpunit_prepare_step_cwd() {
    local cwd_ref="${1:-}"
    local cwd_host

    if [ -z "$cwd_ref" ]; then
        printf '%s\n' "$PLUGIN_PATH"
        return 0
    fi
    if [[ "$cwd_ref" = /* ]] || [[ "$cwd_ref" == *..* ]]; then
        return 1
    fi

    cwd_host="${PLUGIN_PATH}/${cwd_ref}"
    case "$cwd_host" in
        "$PLUGIN_PATH"|"$PLUGIN_PATH"/*)
            [ -d "$cwd_host" ] || return 1
            printf '%s\n' "$cwd_host"
            return 0
            ;;
    esac

    return 1
}

emit_phpunit_prepare_failure_diagnostic() {
    local step_index="$1"
    local command_name="$2"
    local cwd_host="$3"
    local output_file="$4"
    local exit_code="$5"
    local diagnostics_file="${ARTIFACTS_DIR}/wp-codebox-phpunit-prepare-diagnostics.json"
    local output_artifact="${ARTIFACTS_DIR}/wp-codebox-phpunit-prepare-step-${step_index}-output.txt"

    mkdir -p "$ARTIFACTS_DIR"
    cp "$output_file" "$output_artifact"

    jq -n \
        --arg schema "homeboy/wordpress-phpunit-diagnostic/v1" \
        --arg code "wp-codebox-phpunit-prepare-failed" \
        --arg message "WP Codebox PHPUnit prepare step failed before the plugin was mounted into WordPress." \
        --arg command "$command_name" \
        --arg cwd "$cwd_host" \
        --argjson stepIndex "$step_index" \
        --argjson exitCode "$exit_code" \
        --arg artifactsDir "$ARTIFACTS_DIR" \
        --arg outputArtifact "$output_artifact" \
        '{
            schema: $schema,
            diagnostics: [{
                code: $code,
                severity: "error",
                phase: "prepare",
                message: $message,
                step_index: $stepIndex,
                command: $command,
                cwd: $cwd,
                exit_code: $exitCode,
                artifacts: {
                    directory: $artifactsDir,
                    output: $outputArtifact
                }
            }]
        }' > "$diagnostics_file"

    echo "WP Codebox PHPUnit prepare step failed before plugin runtime launch." >&2
    echo "  Step: ${step_index}" >&2
    echo "  Command: ${command_name}" >&2
    echo "  CWD: ${cwd_host}" >&2
    echo "  Exit code: ${exit_code}" >&2
    echo "  Diagnostics: ${diagnostics_file}" >&2
    echo "  Raw output: ${output_artifact}" >&2
}

run_phpunit_prepare_steps() {
    if ! printf '%s' "$WP_CODEBOX_PREPARE_STEPS_JSON" | jq -e 'type == "array" and length > 0' >/dev/null 2>&1; then
        return 0
    fi

    local index=0
    local step_json command_name cwd_ref cwd_host output_file exit_code
    while IFS= read -r step_json; do
        command_name=$(printf '%s' "$step_json" | jq -r '.command')
        cwd_ref=$(printf '%s' "$step_json" | jq -r '.cwd // empty')
        if ! cwd_host=$(phpunit_prepare_step_cwd "$cwd_ref"); then
            echo "Error: wp_codebox_prepare_steps[$index].cwd must be a directory under component root." >&2
            FAILED_STEP="WP Codebox PHPUnit prepare setup"
            exit 1
        fi

        output_file=$(mktemp "${TMPDIR:-/tmp}/homeboy-wp-codebox-phpunit-prepare-${index}.XXXXXX")
        echo "Preparing WordPress PHPUnit plugin source: ${command_name}" >&2
        set +e
        (
            cd "$cwd_host"
            step_args=()
            while IFS= read -r step_arg; do
                step_args+=("$step_arg")
            done < <(printf '%s' "$step_json" | jq -r '(.args // [])[]')
            "$command_name" "${step_args[@]}"
        ) >"$output_file" 2>&1
        exit_code=$?
        set -e

        if [ "$exit_code" -ne 0 ]; then
            emit_phpunit_prepare_failure_diagnostic "$index" "$command_name" "$cwd_host" "$output_file" "$exit_code"
            rm -f "$output_file"
            FAILED_STEP="WP Codebox PHPUnit prepare step"
            exit "$exit_code"
        fi

        rm -f "$output_file"
        index=$((index + 1))
    done < <(printf '%s' "$WP_CODEBOX_PREPARE_STEPS_JSON" | jq -c '.[]')
}

compile_phpunit_prepare_steps
append_phpunit_component_composer_prepare_step
apply_phpunit_component_prepare_profile

detect_phpunit_project_bootstrap() {
    local config_file
    for config_file in "${PLUGIN_PATH}/phpunit.xml" "${PLUGIN_PATH}/phpunit.xml.dist"; do
        [ -f "$config_file" ] || continue

        php -r '
            $xml = file_get_contents($argv[1]);
            if (!is_string($xml)) {
                exit(1);
            }
            if (preg_match("/<phpunit\\b[^>]*\\sbootstrap=([\\\"\\x27])(.*?)\\1/is", $xml, $matches)) {
                echo html_entity_decode($matches[2], ENT_QUOTES | ENT_XML1, "UTF-8");
                exit(0);
            }
            exit(1);
        ' "$config_file" 2>/dev/null && return 0
    done

    return 1
}

validate_project_bootstrap_ref() {
    local bootstrap_ref="$1"

    if [ -z "$bootstrap_ref" ]; then
        echo "Error: wp_codebox_phpunit_bootstrap_mode=project requires a project bootstrap path from wp_codebox_phpunit_project_bootstrap or phpunit.xml(.dist)." >&2
        FAILED_STEP="WP Codebox PHPUnit bootstrap mode setup"
        exit 1
    fi
    if [[ "$bootstrap_ref" = /* ]] || [[ "$bootstrap_ref" == *..* ]]; then
        echo "Error: project PHPUnit bootstrap must be component-relative and stay under component root: $bootstrap_ref" >&2
        FAILED_STEP="WP Codebox PHPUnit bootstrap mode setup"
        exit 1
    fi
    if [ ! -f "${PLUGIN_PATH}/${bootstrap_ref}" ]; then
        echo "Error: project PHPUnit bootstrap file not found: ${PLUGIN_PATH}/${bootstrap_ref}" >&2
        FAILED_STEP="WP Codebox PHPUnit bootstrap mode setup"
        exit 1
    fi
}

configure_phpunit_bootstrap_mode() {
    local settings_json="${HOMEBOY_SETTINGS_JSON:-}"
    [ -n "$settings_json" ] || settings_json="{}"

    WP_CODEBOX_PHPUNIT_BOOTSTRAP_MODE=$(printf '%s' "$settings_json" | jq -r '.wp_codebox_phpunit_bootstrap_mode // "auto"' 2>/dev/null || printf 'auto')
    WP_CODEBOX_PHPUNIT_PROJECT_BOOTSTRAP=$(printf '%s' "$settings_json" | jq -r '.wp_codebox_phpunit_project_bootstrap // empty' 2>/dev/null || true)

    case "$WP_CODEBOX_PHPUNIT_BOOTSTRAP_MODE" in
        managed|project|auto)
            ;;
        *)
            echo "Error: wp_codebox_phpunit_bootstrap_mode must be one of managed, project, or auto (got '$WP_CODEBOX_PHPUNIT_BOOTSTRAP_MODE')." >&2
            FAILED_STEP="WP Codebox PHPUnit bootstrap mode setup"
            exit 1
            ;;
    esac

    if [ -z "$WP_CODEBOX_PHPUNIT_PROJECT_BOOTSTRAP" ]; then
        WP_CODEBOX_PHPUNIT_PROJECT_BOOTSTRAP=$(detect_phpunit_project_bootstrap || true)
    fi

    if [ "$WP_CODEBOX_PHPUNIT_BOOTSTRAP_MODE" = "auto" ]; then
        if [ -n "$WP_CODEBOX_PHPUNIT_PROJECT_BOOTSTRAP" ]; then
            WP_CODEBOX_PHPUNIT_BOOTSTRAP_MODE="project"
        else
            WP_CODEBOX_PHPUNIT_BOOTSTRAP_MODE="managed"
        fi
    fi

    if [ "$WP_CODEBOX_PHPUNIT_BOOTSTRAP_MODE" = "project" ]; then
        validate_project_bootstrap_ref "$WP_CODEBOX_PHPUNIT_PROJECT_BOOTSTRAP"
    else
        WP_CODEBOX_PHPUNIT_PROJECT_BOOTSTRAP=""
    fi
}

configure_phpunit_bootstrap_mode

if [ -n "${COMPONENT_ID:-}" ]; then
    export HOMEBOY_COMPONENT_ID="$COMPONENT_ID"
    export HOMEBOY_COMPONENT_PATH="$PLUGIN_PATH"
    export HOMEBOY_PLUGIN_PATH="$PLUGIN_PATH"
else
    export HOMEBOY_PROJECT_PATH="$PLUGIN_PATH"
    export HOMEBOY_PLUGIN_PATH="$PLUGIN_PATH"
fi

if type homeboy_export_validation_dependency_paths &>/dev/null; then
    homeboy_export_validation_dependency_paths "$PLUGIN_PATH"
fi
DEPENDENCY_PATHS="${HOMEBOY_WORDPRESS_DEPENDENCY_PATHS:-}"

WP_CONFIG_DEFINES_JSON="{}"
PHPUNIT_ENV_JSON="{}"
WP_CODEBOX_FILE_MOUNTS_JSON="[]"
WP_CODEBOX_COMMAND_DIAGNOSTICS_JSON="null"
PHPUNIT_NO_TESTS="skipped"
WP_CODEBOX_WORDPRESS_VERSION=""
WP_CODEBOX_MULTISITE=""
if [ -n "${HOMEBOY_SETTINGS_JSON:-}" ] && [ "${HOMEBOY_SETTINGS_JSON}" != "{}" ]; then
    extracted=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -c '.wp_config_defines // {}' 2>/dev/null || echo "{}")
    [ -n "$extracted" ] && WP_CONFIG_DEFINES_JSON="$extracted"

    extracted=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -c '.wp_codebox_file_mounts // []' 2>/dev/null || echo "[]")
    [ -n "$extracted" ] && [ "$extracted" != "null" ] && WP_CODEBOX_FILE_MOUNTS_JSON="$extracted"

    extracted=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -r '.phpunit_no_tests // empty' 2>/dev/null || true)
    [ -n "$extracted" ] && [ "$extracted" != "null" ] && PHPUNIT_NO_TESTS="$extracted"

    extracted=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -r '.wordpress_runtime_version // .wp_codebox_wordpress_version // empty' 2>/dev/null || true)
    [ -n "$extracted" ] && [ "$extracted" != "null" ] && WP_CODEBOX_WORDPRESS_VERSION="$extracted"

    extracted=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -r '.wp_codebox_multisite // empty' 2>/dev/null || true)
    [ -n "$extracted" ] && [ "$extracted" != "null" ] && WP_CODEBOX_MULTISITE="$extracted"

    extracted=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -c '.wp_codebox_command_diagnostics // .command_diagnostics // .diagnostics_capture // .diagnosticsCapture // null' 2>/dev/null || echo "null")
    [ -n "$extracted" ] && WP_CODEBOX_COMMAND_DIAGNOSTICS_JSON="$extracted"
fi
if [ -n "${HOMEBOY_WORDPRESS_MULTISITE+x}" ]; then
    WP_CODEBOX_MULTISITE="$HOMEBOY_WORDPRESS_MULTISITE"
fi
if [ -z "$WP_CODEBOX_MULTISITE" ] && detect_network_plugin_header; then
    WP_CODEBOX_MULTISITE="1"
fi

CHANGED_TEST_FILES_JSON="[]"
if [ -n "${HOMEBOY_CHANGED_TEST_FILES:-}" ]; then
    CHANGED_TEST_FILES_JSON=$(printf '%s' "${HOMEBOY_CHANGED_TEST_FILES}" | php -r '
        $files = array_values(array_filter(array_map("trim", explode("\n", stream_get_contents(STDIN)))));
        echo json_encode($files, JSON_UNESCAPED_SLASHES);
    ' 2>/dev/null || printf '[]')
fi
PHPUNIT_ARGS_JSON=$(printf '%s\0' "${PASSTHROUGH_ARGS[@]}" | php -r '
    $raw = stream_get_contents(STDIN);
    $parts = array_values(array_filter(explode("\0", $raw), static function ($value) { return $value !== ""; }));
    echo json_encode($parts, JSON_UNESCAPED_SLASHES);
' 2>/dev/null || printf '[]')

SELECTED_TEST_FILE_REL=""
if [ -n "$SELECTED_TEST_FILE" ]; then
    if [ "${SELECTED_TEST_FILE#/}" != "$SELECTED_TEST_FILE" ]; then
        selected_abs="$SELECTED_TEST_FILE"
    else
        selected_abs="${PLUGIN_PATH}/${SELECTED_TEST_FILE}"
    fi
    if [ ! -f "$selected_abs" ]; then
        echo "ERROR: requested PHPUnit test file not found: ${SELECTED_TEST_FILE}" >&2
        exit 2
    fi
    case "$selected_abs" in
        "${PLUGIN_PATH}"/tests/*.php|"${PLUGIN_PATH}"/tests/*/*.php|"${PLUGIN_PATH}"/tests/*/*/*.php|"${PLUGIN_PATH}"/tests/*/*/*/*.php)
            SELECTED_TEST_FILE_REL="${selected_abs#"${PLUGIN_PATH}/"}"
            ;;
        *)
            echo "ERROR: requested PHPUnit test file must live under tests/: ${SELECTED_TEST_FILE}" >&2
            exit 2
            ;;
    esac
fi

MOUNTS_JSON="[]"
EXTRA_PLUGINS_JSON="[]"
homeboy_wp_codebox_add_recipe_mount() {
    local source="$1"
    local target="$2"
    local mode="${3:-readwrite}"
    MOUNTS_JSON=$(jq -nc --argjson mounts "$MOUNTS_JSON" --arg source "$source" --arg target "$target" --arg mode "$mode" '$mounts + [{source: $source, target: $target, mode: $mode}]')
}

if [ -n "$WP_CODEBOX_SOURCE_ROOT" ]; then
    EXTRA_PLUGINS_JSON=$(jq -nc \
        --arg source "$WP_CODEBOX_SOURCE_ROOT" \
        --arg sourceSubpath "$WP_CODEBOX_SOURCE_SUBPATH" \
        --arg slug "$PLUGIN_SLUG" \
        '[{source: $source, sourceRoot: $source, sourceSubpath: $sourceSubpath, slug: $slug, activate: false}]')
else
    homeboy_wp_codebox_add_recipe_mount "${PLUGIN_PATH}" "/wordpress/wp-content/plugins/${PLUGIN_SLUG}"
fi

if [ -n "$DEPENDENCY_PATHS" ]; then
    while IFS= read -r dep_path; do
        [ -z "$dep_path" ] && continue
        dep_slug="$(homeboy_get_validation_dependency_slug "$dep_path" || basename "$dep_path")"
        homeboy_wp_codebox_add_recipe_mount "${dep_path}" "/wordpress/wp-content/plugins/${dep_slug}"
    done <<< "$DEPENDENCY_PATHS"
fi

PLUGIN_DB_PHP="${PLUGIN_PATH}/db.php"
if [ -f "$PLUGIN_DB_PHP" ]; then
    homeboy_wp_codebox_add_recipe_mount "${PLUGIN_DB_PHP}" "/wordpress/wp-content/db.php"
fi

if printf '%s' "$WP_CODEBOX_FILE_MOUNTS_JSON" | jq -e 'type == "array" and length > 0' >/dev/null 2>&1; then
    while IFS= read -r mount_json; do
        [ -n "$mount_json" ] || continue
        mount_from=$(printf '%s' "$mount_json" | jq -r '.from // empty')
        mount_to=$(printf '%s' "$mount_json" | jq -r '.to // empty')
        mount_dependency=$(printf '%s' "$mount_json" | jq -r '.from_dependency // empty')
        if [ -z "$mount_from" ] || [ -z "$mount_to" ]; then
            echo "Error: wp_codebox_file_mounts entries require 'from' and 'to'" >&2
            FAILED_STEP="WP Codebox file mount setup"
            exit 1
        fi
        if [[ "$mount_from" = /* ]] || [[ "$mount_from" == *..* ]]; then
            echo "Error: wp_codebox_file_mounts 'from' must be a relative path without '..' (got '$mount_from')" >&2
            FAILED_STEP="WP Codebox file mount setup"
            exit 1
        fi
        if [[ "$mount_to" != /* ]]; then
            echo "Error: wp_codebox_file_mounts 'to' must be an absolute WP Codebox sandbox path (got '$mount_to')" >&2
            FAILED_STEP="WP Codebox file mount setup"
            exit 1
        fi

        mount_root="$PLUGIN_PATH"
        if [ -n "$mount_dependency" ]; then
            mount_root=""
            if [ -n "$DEPENDENCY_PATHS" ]; then
                while IFS= read -r dep_path; do
                    [ -z "$dep_path" ] && continue
                    dep_slug="$(homeboy_get_validation_dependency_slug "$dep_path" || basename "$dep_path")"
                    if [ "$dep_slug" = "$mount_dependency" ] || [ "$(basename "$dep_path")" = "$mount_dependency" ]; then
                        mount_root="$dep_path"
                        break
                    fi
                done <<< "$DEPENDENCY_PATHS"
            fi
            if [ -z "$mount_root" ]; then
                echo "Error: wp_codebox_file_mounts dependency not found: $mount_dependency" >&2
                FAILED_STEP="WP Codebox file mount setup"
                exit 1
            fi
        fi

        mount_host="${mount_root}/${mount_from}"
        if [ ! -f "$mount_host" ]; then
            echo "Error: wp_codebox_file_mounts source file not found: $mount_host" >&2
            FAILED_STEP="WP Codebox file mount setup"
            exit 1
        fi
        homeboy_wp_codebox_add_recipe_mount "${mount_host}" "${mount_to}"
    done < <(printf '%s' "$WP_CODEBOX_FILE_MOUNTS_JSON" | jq -c '.[]')
fi

EXTENSION_VENDOR_PATH="$(homeboy_wp_codebox_resolve_mount_path "${EXTENSION_PATH}/vendor")"
homeboy_wp_codebox_add_recipe_mount "${EXTENSION_VENDOR_PATH}" "/wp-codebox-vendor" "readonly"
EXTENSION_MOUNT_PATH="$(homeboy_wp_codebox_resolve_mount_path "${EXTENSION_PATH}")"
homeboy_wp_codebox_add_recipe_mount "${EXTENSION_MOUNT_PATH}" "/homeboy-extension" "readonly"

WP_CODEBOX_DEP_MOUNTS=""
if [ -n "$DEPENDENCY_PATHS" ]; then
    while IFS= read -r dep_path; do
        [ -z "$dep_path" ] && continue
        dep_slug="$(homeboy_get_validation_dependency_slug "$dep_path" || basename "$dep_path")"
        if [ -n "$WP_CODEBOX_DEP_MOUNTS" ]; then
            WP_CODEBOX_DEP_MOUNTS+="\\n"
        fi
        WP_CODEBOX_DEP_MOUNTS+="/wordpress/wp-content/plugins/${dep_slug}"
    done <<< "$DEPENDENCY_PATHS"
fi

RESULT_FILE="${PLUGIN_PATH}/.pg-test-result.txt"
PHPUNIT_RESULT_CACHE_FILE="${PLUGIN_PATH}/.phpunit.result.cache"

cleanup_wp_codebox_phpunit_runtime_files() {
    rm -f "$RESULT_FILE" "$PHPUNIT_RESULT_CACHE_FILE"
}

trap cleanup_wp_codebox_phpunit_runtime_files EXIT
cleanup_wp_codebox_phpunit_runtime_files

ARTIFACTS_DIR="${HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR:-}"
if [ -z "$ARTIFACTS_DIR" ] && [ -n "${HOMEBOY_SETTINGS_JSON:-}" ] && [ "${HOMEBOY_SETTINGS_JSON}" != "{}" ]; then
    ARTIFACTS_DIR=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -r '.wp_codebox_artifacts_dir // empty' 2>/dev/null || true)
fi
if [ -z "$ARTIFACTS_DIR" ]; then
    ARTIFACTS_DIR=$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wp-codebox-test-artifacts.XXXXXX")
fi

run_phpunit_prepare_steps

echo "Running PHPUnit tests via WP Codebox..."
echo "  Plugin: ${PLUGIN_SLUG} (${PLUGIN_PATH})"
echo "  Backend: wp-codebox"

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "  Mounts: ${MOUNTS_JSON}"
    echo "  WordPress version: ${WP_CODEBOX_WORDPRESS_VERSION}"
    echo "  Multisite: ${WP_CODEBOX_MULTISITE:-0}"
    echo "  Artifacts: ${ARTIFACTS_DIR}"
fi

WP_CODEBOX_TMPFILE=$(mktemp)
PHPUNIT_STDOUT_TMPFILE=$(mktemp)
RECIPE_FILE=$(mktemp "${TMPDIR:-/tmp}/homeboy-wp-codebox-test-recipe.XXXXXX")
RECIPE_OPTIONS_FILE=$(mktemp "${TMPDIR:-/tmp}/homeboy-wp-codebox-test-recipe-options.XXXXXX")
homeboy_wp_codebox_set_command "$WP_CODEBOX_BIN"
wp_codebox_command=("${HOMEBOY_WP_CODEBOX_COMMAND[@]}")

jq -n \
    --arg wp "$WP_CODEBOX_WORDPRESS_VERSION" \
    --argjson extraPlugins "$EXTRA_PLUGINS_JSON" \
    --argjson mounts "$MOUNTS_JSON" \
    --arg pluginSlug "$PLUGIN_SLUG" \
    --arg selectedTestFile "$SELECTED_TEST_FILE_REL" \
    --argjson changedTests "$CHANGED_TEST_FILES_JSON" \
    --argjson phpunitArgs "$PHPUNIT_ARGS_JSON" \
    --argjson env "$PHPUNIT_ENV_JSON" \
    --argjson defines "$WP_CONFIG_DEFINES_JSON" \
    --argjson bootstrapFiles "$WP_CODEBOX_BOOTSTRAP_FILES_JSON" \
    --arg bootstrapMode "$WP_CODEBOX_PHPUNIT_BOOTSTRAP_MODE" \
    --arg projectBootstrap "$WP_CODEBOX_PHPUNIT_PROJECT_BOOTSTRAP" \
    --arg dependencyMounts "$WP_CODEBOX_DEP_MOUNTS" \
    --arg multisite "$WP_CODEBOX_MULTISITE" \
    --argjson diagnostics "$WP_CODEBOX_COMMAND_DIAGNOSTICS_JSON" \
    '({
        extra_plugins: $extraPlugins,
        mounts: $mounts,
        pluginSlug: $pluginSlug,
        selectedTestFile: $selectedTestFile,
        changedTestFiles: $changedTests,
        phpunitArgs: $phpunitArgs,
        env: $env,
        wpConfigDefines: $defines,
        bootstrapFiles: $bootstrapFiles,
        bootstrapMode: $bootstrapMode,
        projectBootstrap: $projectBootstrap,
        autoloadFile: "/wp-codebox-vendor/autoload.php",
        testsDir: "/wp-codebox-vendor/wp-phpunit/wp-phpunit",
        dependencyMounts: ($dependencyMounts | split("\n") | map(select(. != ""))),
        multisite: (if (($multisite | ascii_downcase) as $v | $v == "1" or $v == "true" or $v == "yes" or $v == "on") then true else false end)
    }
    + (if $wp == "" then {} else {wordpressVersion: $wp} end)
    + (if $diagnostics == null then {} else {diagnosticsCapture: $diagnostics} end))' > "$RECIPE_OPTIONS_FILE"

if [ ! -f "$PHPUNIT_RECIPE_BUILDER" ]; then
    echo "Error: WP Codebox PHPUnit recipe builder not found: ${PHPUNIT_RECIPE_BUILDER}" >&2
    FAILED_STEP="WP Codebox PHPUnit recipe setup"
    exit 1
fi
node "$PHPUNIT_RECIPE_BUILDER" < "$RECIPE_OPTIONS_FILE" > "$RECIPE_FILE"

set +e
"${wp_codebox_command[@]}" recipe-run \
    --recipe "$RECIPE_FILE" \
    --artifacts "$ARTIFACTS_DIR" \
    --json \
    > "$WP_CODEBOX_TMPFILE" 2>&1
wp_codebox_exit=$?
set -e

rm -f "$RECIPE_FILE" "$RECIPE_OPTIONS_FILE"

WP_CODEBOX_OUTPUT=$(cat "$WP_CODEBOX_TMPFILE")
PHPUNIT_OUTPUT=""
if [ -f "$RESULT_FILE" ]; then
    PHPUNIT_OUTPUT=$(cat "$RESULT_FILE")
fi
PHPUNIT_STDOUT=$(jq -r '(.executions // [])[-1].stdout // empty' "$WP_CODEBOX_TMPFILE" 2>/dev/null || true)
printf '%s\n' "$PHPUNIT_STDOUT" > "$PHPUNIT_STDOUT_TMPFILE"

if [ -n "$WP_CODEBOX_OUTPUT" ]; then
    if echo "$PHPUNIT_OUTPUT" | grep -q "^NO_TEST_FILES" && [ "$PHPUNIT_NO_TESTS" != "failed" ] && [ "$PHPUNIT_NO_TESTS" != "fail" ] && [ ! -f "${PLUGIN_PATH}/phpunit.xml" ] && [ ! -f "${PLUGIN_PATH}/phpunit.xml.dist" ]; then
        :
    else
        jq -r '(.executions // [])[-1].stdout // empty' "$WP_CODEBOX_TMPFILE" 2>/dev/null || cat "$WP_CODEBOX_TMPFILE"
    fi
fi

PARSE_RESULTS="${EXTENSION_PATH}/scripts/test/parse-test-results.sh"
PARSE_FAILURES="${EXTENSION_PATH}/scripts/test/parse-test-failures.sh"
if [ -n "${HOMEBOY_TEST_RESULTS_FILE:-}" ] && [ -f "$PARSE_RESULTS" ]; then
	if [ -f "${ARTIFACTS_DIR}/files/test-results.json" ]; then
		bash "$PARSE_RESULTS" "$ARTIFACTS_DIR" || true
	elif [ -n "$PHPUNIT_STDOUT" ]; then
		bash "$PARSE_RESULTS" "$PHPUNIT_STDOUT_TMPFILE" || true
	elif [ -n "$PHPUNIT_OUTPUT" ]; then
		bash "$PARSE_RESULTS" "$RESULT_FILE" || true
	fi
fi
if [ -n "${HOMEBOY_TEST_FAILURES_FILE:-}" ] && [ -f "$PARSE_FAILURES" ]; then
	if [ -f "${ARTIFACTS_DIR}/files/test-results.json" ]; then
		bash "$PARSE_FAILURES" "$ARTIFACTS_DIR" "${PLUGIN_PATH:-}" || true
	elif [ -n "$PHPUNIT_STDOUT" ]; then
		bash "$PARSE_FAILURES" "$PHPUNIT_STDOUT_TMPFILE" "${PLUGIN_PATH:-}" || true
	fi
fi
rm -f "$WP_CODEBOX_TMPFILE" "$PHPUNIT_STDOUT_TMPFILE"

dump_diagnostics() {
    local label="$1"
    echo ""
    echo "============================================"
    echo "$label"
    echo "============================================"
    if [ -n "$PHPUNIT_OUTPUT" ]; then
        echo ""
        echo "--- Structured log ($RESULT_FILE) ---"
        echo "$PHPUNIT_OUTPUT"
    fi
    if [ -n "$PHPUNIT_STDOUT" ]; then
        echo ""
        echo "--- WP Codebox stdout ---"
        echo "$PHPUNIT_STDOUT"
    fi
}

is_changed_since_registration_drift() {
    if [ -z "${HOMEBOY_CHANGED_SINCE:-}" ]; then
        return 1
    fi

    local registration_output
    registration_output="${PHPUNIT_STDOUT}
${WP_CODEBOX_OUTPUT}"

    if echo "$registration_output" | grep -qE "Abilities not registered during plugin boot|Ability category '.+' should be registered during plugin boot|WP_Abilities_Registry::get_registered|Ability .* not found"; then
        return 0
    fi

    local drift_count
    drift_count=$(echo "$registration_output" | grep -Ec "Failed asserting that an array has the key '([^']+)'." || true)
    if [ "${drift_count:-0}" -ge 3 ]; then
        return 0
    fi

    return 1
}

dump_registration_drift_preflight() {
    local registration_output
    registration_output="${PHPUNIT_STDOUT}
${WP_CODEBOX_OUTPUT}"

    dump_diagnostics "HARNESS PREFLIGHT FAILURE: WordPress bootstrap registration drift"
    echo ""
    echo "Changed-since PHPUnit hit broad missing registration drift in the WordPress test runtime."
    echo "This is reported as one harness/preflight failure so unrelated ability, task, and tool tests do not mask the branch signal."
    echo "  changed-since: ${HOMEBOY_CHANGED_SINCE}"
    echo ""
    echo "--- Registration drift evidence ---"
    echo "$registration_output" | grep -E "Abilities not registered during plugin boot|Ability category '.+' should be registered during plugin boot|WP_Abilities_Registry::get_registered|Ability .* not found|Failed asserting that an array has the key '([^']+)'." | head -20 || true
}

if echo "$PHPUNIT_OUTPUT" | grep -qE '^STAGE_(FAIL|FATAL):'; then
    FAILED_STAGE_LINE=$(echo "$PHPUNIT_OUTPUT" | grep -E '^STAGE_(FAIL|FATAL):' | head -1)
    FAILED_STAGE_DETAIL=$(echo "$FAILED_STAGE_LINE" | sed -E 's/^STAGE_(FAIL|FATAL)://')
    FAILED_STEP="WP Codebox bootstrap (${FAILED_STAGE_DETAIL%%:*} stage)"
    FAILURE_OUTPUT="$FAILED_STAGE_LINE"
    dump_diagnostics "BOOTSTRAP FAILURE: $FAILED_STAGE_DETAIL"
    rm -f "$RESULT_FILE"
    exit ${wp_codebox_exit:-1}
fi

if [ $wp_codebox_exit -ne 0 ] && is_changed_since_registration_drift; then
    FAILED_STEP="WordPress PHPUnit harness preflight (registration drift)"
    FAILURE_OUTPUT="Changed-since WordPress PHPUnit detected broad missing registration drift."
    dump_registration_drift_preflight
    write_phpunit_discovery_result failed "wordpress-registration-drift" "Changed-since WordPress PHPUnit detected broad missing registration drift in the test runtime."
    rm -f "$RESULT_FILE"
    exit 1
fi

if echo "$PHPUNIT_OUTPUT" | grep -q "SOME TESTS FAILED"; then
    FAILED_STEP="PHPUnit tests (wp-codebox backend)"
    FAILURE_REPLAY_MODE="none"
    rm -f "$RESULT_FILE"
    exit ${wp_codebox_exit:-1}
fi

if echo "$PHPUNIT_STDOUT" | grep -q 'Error in bootstrap script:'; then
    FAILED_STEP="PHPUnit bootstrap failure (wp-codebox)"
    FAILURE_OUTPUT=$(echo "$PHPUNIT_STDOUT" | grep 'Error in bootstrap script:' | head -1)
    dump_diagnostics "PHPUNIT BOOTSTRAP FAILURE"
    write_phpunit_discovery_result failed "phpunit-bootstrap-failure" "PHPUnit bootstrap failed before executing tests."
    rm -f "$RESULT_FILE"
    exit 1
fi

if [ $wp_codebox_exit -ne 0 ] && echo "$PHPUNIT_STDOUT" | grep -qE '^(FAILURES|ERRORS)!'; then
    FAILED_STEP="PHPUnit tests (wp-codebox backend)"
    FAILURE_REPLAY_MODE="none"
    rm -f "$RESULT_FILE"
    exit $wp_codebox_exit
fi

if [ $wp_codebox_exit -ne 0 ] && echo "$PHPUNIT_STDOUT" | grep -qE '^(PHP Parse error|Parse error:|PHP Fatal error|Fatal error:)'; then
    FAILED_STEP="WP Codebox PHP crash (before runner took control)"
    FAILURE_OUTPUT=$(echo "$PHPUNIT_STDOUT" | grep -E '^(PHP Parse error|Parse error:|PHP Fatal error|Fatal error:)' | head -5)
    dump_diagnostics "PHP CRASH"
    rm -f "$RESULT_FILE"
    exit $wp_codebox_exit
fi

if echo "$PHPUNIT_OUTPUT" | grep -q "^NO_TEST_FILES"; then
    if component_has_composer_test_script; then
        rm -f "$RESULT_FILE"
        run_composer_test_script
        exit $?
    fi

    if component_npm_test_script; then
        rm -f "$RESULT_FILE"
        run_npm_test_script
        exit $?
    fi

    if [ "$PHPUNIT_NO_TESTS" = "failed" ] || [ "$PHPUNIT_NO_TESTS" = "fail" ] || [ -f "${PLUGIN_PATH}/phpunit.xml" ] || [ -f "${PLUGIN_PATH}/phpunit.xml.dist" ]; then
        dump_diagnostics "NO PHPUNIT TEST FILES DISCOVERED"
        echo ""
        if [ "$PHPUNIT_NO_TESTS" = "failed" ] || [ "$PHPUNIT_NO_TESTS" = "fail" ]; then
            echo "PHPUnit no-test discovery is configured as failure, and no files matched the WordPress runner discovery contract."
        else
            echo "PHPUnit config exists, but no files matched the WordPress runner discovery contract."
        fi
        echo "  Check phpunit.xml(.dist), tests/ directory layout, and Test.php/test- naming."
        FAILED_STEP="PHPUnit tests (configured suite discovered no test files, wp-codebox)"
        write_phpunit_discovery_result failed "no-phpunit-tests-configured" "Plugin activation/install passed; PHPUnit discovery found zero tests; no PHPUnit assertions ran."
        rm -f "$RESULT_FILE"
        exit 1
    fi

    echo ""
    echo "Skipping PHPUnit tests: plugin activation/install passed, but no files matched the WordPress runner discovery contract."
    echo "  Contract: files under ${TEST_DIR} ending in Test.php or starting with test-."
    echo "  PHPUnit discovery found zero tests; no PHPUnit assertions ran."
    echo "  Add matching PHPUnit files or a component phpunit.xml(.dist) if this suite should run here."
    write_phpunit_discovery_result skipped "no-phpunit-tests" "Plugin activation/install passed; PHPUnit discovery found zero tests; no PHPUnit assertions ran."
    rm -f "$RESULT_FILE"
    exit 0
fi

if [ $wp_codebox_exit -ne 0 ]; then
    FAILED_STEP="WP Codebox exited with code $wp_codebox_exit (unclassified)"
    dump_diagnostics "UNCLASSIFIED WP CODEBOX FAILURE (exit=$wp_codebox_exit)"
    rm -f "$RESULT_FILE"
    exit $wp_codebox_exit
fi

if [ -z "$PHPUNIT_OUTPUT" ] && [ -z "$PHPUNIT_STDOUT" ]; then
    dump_diagnostics "NO OUTPUT CAPTURED"
    FAILED_STEP="PHPUnit tests (no output, wp-codebox)"
    rm -f "$RESULT_FILE"
    exit 1
fi

if echo "$PHPUNIT_STDOUT" | grep -qE 'No tests executed|OK \(0 tests'; then
    dump_diagnostics "ZERO TESTS EXECUTED"
    FAILED_STEP="PHPUnit tests (zero tests executed, wp-codebox)"
    rm -f "$RESULT_FILE"
    exit 1
fi

if echo "$PHPUNIT_OUTPUT" | grep -q "^NOTICE:"; then
    echo ""
    echo "--- Bootstrap notices (non-fatal) ---"
    echo "$PHPUNIT_OUTPUT" | grep "^NOTICE:"
fi

rm -f "$RESULT_FILE"

echo ""
echo "WP Codebox test run complete."
