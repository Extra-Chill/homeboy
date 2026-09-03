#!/usr/bin/env bash
set -euo pipefail

# Test runner router for WordPress Homeboy extension.
#
# Plugin/theme PHPUnit tests and core-dev checkouts run through the selected
# WordPress runtime backend. PHP smoke scripts can declare a standalone PHP
# environment in the component's test manifest.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SHARED_LIB_DIR="${HOMEBOY_SHARED_LIB_DIR:-}"
if [ -z "$SHARED_LIB_DIR" ] && [ -n "${HOMEBOY_EXTENSION_PATH:-}" ] && [ -d "${HOMEBOY_EXTENSION_PATH}/../scripts/lib" ]; then
    SHARED_LIB_DIR="$(cd "${HOMEBOY_EXTENSION_PATH}/../scripts/lib" && pwd)"
fi
SHARED_LIB_DIR="${SHARED_LIB_DIR:-$(cd "${SCRIPT_DIR}/../../../scripts/lib" && pwd)}"

# shellcheck source=/dev/null
source "${SHARED_LIB_DIR}/runner-harness.sh"
homeboy_runner_harness_init --bash 4 --component-alias PLUGIN_PATH

SMOKE_RUNNER="${HOMEBOY_RUNTIME_TEST_RUNNER_HOST_SMOKE_WP:-${SCRIPT_DIR}/test-runner-host-smoke-wp.sh}"
WP_CODEBOX_RUNNER="${HOMEBOY_RUNTIME_TEST_RUNNER_WP_CODEBOX:-${SCRIPT_DIR}/test-runner-wp-codebox.sh}"
CORE_WP_CODEBOX_RUNNER="${HOMEBOY_RUNTIME_TEST_RUNNER_CORE_WP_CODEBOX:-${SCRIPT_DIR}/test-runner-core-dev-wp-codebox.sh}"
WORDPRESS_TEST_RUNTIME_BACKEND="${HOMEBOY_WORDPRESS_TEST_RUNTIME_BACKEND:-wp-codebox}"

SETTINGS_HELPER="$(homeboy_runner_harness_resolve_helper HOMEBOY_RUNTIME_SETTINGS_HELPER settings.sh)" || exit 1
PROJECT_SCRIPTS_HELPER="${HOMEBOY_RUNTIME_PROJECT_SCRIPTS_HELPER:-${SHARED_LIB_DIR}/project-scripts.sh}"
# shellcheck source=/dev/null
source "$SETTINGS_HELPER"
if [ -f "$PROJECT_SCRIPTS_HELPER" ]; then
    # shellcheck source=/dev/null
    source "$PROJECT_SCRIPTS_HELPER"
fi

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: Extension path: $EXTENSION_PATH"
    echo "DEBUG: Component: ${HOMEBOY_COMPONENT_ID:-none}"
    echo "DEBUG: Component path: ${COMPONENT_PATH:-$(pwd)}"
fi

homeboy_wordpress_discover_phpunit_files() {
    local output_file="$1"
    local discovery_tmp
    discovery_tmp="$(mktemp)" || return 1
    if ! HOMEBOY_WORDPRESS_PHPUNIT_DISCOVERY_ONLY=1 bash "$WP_CODEBOX_RUNNER" > "$discovery_tmp"; then
        rm -f "$discovery_tmp"
        echo "ERROR: WP Codebox canonical PHPUnit discovery failed." >&2
        return 1
    fi
    if ! jq -e '.schema == "wp-codebox/phpunit-discovery/v1" and (.files | type == "array" and length > 0)' "$discovery_tmp" >/dev/null 2>&1; then
        rm -f "$discovery_tmp"
        echo "ERROR: WP Codebox returned an invalid canonical PHPUnit discovery result." >&2
        return 1
    fi
    mv "$discovery_tmp" "$output_file"
}

# Inventory-only mode enumerates the suite without running any of it, so that
# Homeboy can plan bounded shards. It must return before any runner is selected:
# a producer that executed tests would defeat the point, and core deliberately
# withholds the changed-scope environment here because an inventory is a
# complete enumeration rather than a changed-test execution.
#
# The document is written by a Python producer, not by this shell, because the
# `inventory_fingerprint` contract is literally Python's
# `json.dumps(..., sort_keys=True, separators=(",", ":"))` byte layout, which
# core reproduces by hand when it re-derives the fingerprint. (#12394)
if [ "${HOMEBOY_TEST_INVENTORY_ONLY:-}" = "1" ]; then
    if [ -z "${HOMEBOY_TEST_INVENTORY_FILE:-}" ]; then
        echo "Error: HOMEBOY_TEST_INVENTORY_ONLY is set without HOMEBOY_TEST_INVENTORY_FILE." >&2
        exit 2
    fi
    INVENTORY_TOOL="${SCRIPT_DIR}/test-inventory.py"
    if [ ! -f "$INVENTORY_TOOL" ]; then
        echo "Error: WordPress test inventory producer is missing: ${INVENTORY_TOOL}" >&2
        exit 2
    fi
    inventory_data="$(mktemp)" || {
        echo "Error: could not create temporary WordPress test inventory." >&2
        exit 1
    }
    discovery_data="$(mktemp)" || {
        rm -f "$inventory_data"
        echo "Error: could not create temporary WordPress PHPUnit discovery result." >&2
        exit 1
    }
    if ! homeboy_wordpress_discover_phpunit_files "$discovery_data"; then
        rm -f "$inventory_data" "$discovery_data"
        exit 1
    fi
    if ! python3 "$INVENTORY_TOOL" \
        --project "$PLUGIN_PATH" \
        --extension-path "$EXTENSION_PATH" \
        --runner "${HOMEBOY_WORDPRESS_INVENTORY_RUNNER:-wordpress}" \
        --package "${HOMEBOY_COMPONENT_ID:-wordpress}" \
        --discovery-file "$discovery_data" \
        --output "$inventory_data"; then
        rm -f "$inventory_data" "$discovery_data"
        exit 1
    fi
    if ! cp "$inventory_data" "$HOMEBOY_TEST_INVENTORY_FILE"; then
        rm -f "$inventory_data" "$discovery_data"
        exit 1
    fi
    cat "$inventory_data"
    rm -f "$inventory_data" "$discovery_data"
    exit 0
fi

# WordPress tests run through the selected runtime backend against real
# WordPress. The default suite is PHPUnit. Standalone smoke scripts are
# diagnostic/operator targets and run only when selected explicitly with --file,
# --host-smoke-file, or the HOMEBOY_WORDPRESS_HOST_SMOKE_FILES scope environment.

homeboy_wordpress_test_environment() {
    local test_file="$1"
    local manifest_path="${HOMEBOY_WORDPRESS_TEST_MANIFEST:-${PLUGIN_PATH}/homeboy-test-manifest.json}"

    if [ ! -e "$manifest_path" ]; then
        printf '%s\n' "wordpress"
        return 0
    fi

    jq -er --arg testFile "$test_file" '
        if type != "object" or .schema != "homeboy/test-manifest/v1" then
            error("expected schema homeboy/test-manifest/v1")
        elif (.tests | type) != "object" then
            error("expected tests object")
        else
            (.default_environment // "wordpress") as $defaultEnvironment
            | (.tests[$testFile].environment // $defaultEnvironment) as $environment
            | if $environment == "wordpress" or $environment == "standalone-php" then
                $environment
            else
                error("unsupported environment " + ($environment | tostring))
              end
        end
    ' "$manifest_path" 2>/dev/null || {
        echo "ERROR: invalid WordPress test manifest: ${manifest_path}" >&2
        return 2
    }
}

homeboy_wordpress_run_standalone_php_smoke_files() {
    local smoke_files_raw="$1"
    local php_bin="${HOMEBOY_PHP_BIN:-php}"
    local smoke_file smoke_abs rel_path exit_code
    local passed=0
    local failed=0
    local last_failure_exit=0

    echo "Running standalone PHP smoke tests..."
    echo "  Component: ${HOMEBOY_COMPONENT_ID:-$(basename "$PLUGIN_PATH")} (${PLUGIN_PATH})"
    echo "  Backend: standalone-php"

    while IFS= read -r smoke_file; do
        [ -n "$smoke_file" ] || continue
        if ! smoke_abs="$(homeboy_wordpress_rel_test_file "$smoke_file")"; then
            echo "ERROR: requested standalone PHP smoke file not found or outside the component: ${smoke_file}" >&2
            return 2
        fi
        rel_path="$smoke_abs"
        echo "PHP_SMOKE_BEGIN:${rel_path}"
        if "$php_bin" "${PLUGIN_PATH}/${rel_path}"; then
            echo "PHP_SMOKE_OK:${rel_path}"
            passed=$((passed + 1))
        else
            exit_code=$?
            echo "PHP_SMOKE_FAIL:${rel_path}:exit=${exit_code}"
            failed=$((failed + 1))
            last_failure_exit="$exit_code"
        fi
    done <<< "$smoke_files_raw"

    echo "PHP_SMOKE_SUMMARY:passed=${passed} failed=${failed}"
    WORDPRESS_STANDALONE_PHP_SMOKE_PASSED="$passed"
    WORDPRESS_STANDALONE_PHP_SMOKE_FAILED="$failed"
    [ "$failed" -eq 0 ] || return "$last_failure_exit"
}

show_usage() {
    cat <<'EOF'
Usage: homeboy test <component-id> [-- --file <path>]
       homeboy test <component-id> [-- --host-smoke-file <tests/...-smoke.php>]

Options passed after `--` are handled by the WordPress extension runner:
  --file <path>             Run one test file, routed by file type.
  --host-smoke-file <path>  Run one real-WordPress host smoke through the same
                            WordPress runtime harness used by CI.
                            The file must match tests/**/*-smoke.php.
  --help                    Show this help.

Runtime backend:
  HOMEBOY_WORDPRESS_TEST_RUNTIME_BACKEND selects the real-WordPress runtime
  backend for PHPUnit and core-dev tests. Supported: wp-codebox (default).

Real-WordPress host smokes preserve the HOST_SMOKE_BEGIN,
HOST_SMOKE_PROGRESS, HOST_SMOKE_OK, HOST_SMOKE_FAIL, and
HOST_SMOKE_SUMMARY markers for machine parsing.
EOF
}

homeboy_wordpress_runtime_runner() {
    case "$WORDPRESS_TEST_RUNTIME_BACKEND" in
        wp-codebox)
            printf '%s\n' "$WP_CODEBOX_RUNNER"
            ;;
        *)
            echo "ERROR: unsupported WordPress test runtime backend: ${WORDPRESS_TEST_RUNTIME_BACKEND}" >&2
            echo "  Supported backends: wp-codebox" >&2
            return 2
            ;;
    esac
}

homeboy_wordpress_core_runtime_runner() {
    case "$WORDPRESS_TEST_RUNTIME_BACKEND" in
        wp-codebox)
            printf '%s\n' "$CORE_WP_CODEBOX_RUNNER"
            ;;
        *)
            echo "ERROR: unsupported WordPress core-dev test runtime backend: ${WORDPRESS_TEST_RUNTIME_BACKEND}" >&2
            echo "  Supported backends: wp-codebox" >&2
            return 2
            ;;
    esac
}

TARGET_FILE=""
TARGET_HOST_SMOKE_FILE=""
PASSTHROUGH_ARGS=()
while [ "$#" -gt 0 ]; do
    case "$1" in
        --help|-h)
            show_usage
            exit 0
            ;;
        --file)
            shift
            if [ "$#" -eq 0 ] || [ -z "${1:-}" ]; then
                echo "ERROR: --file requires a path" >&2
                exit 2
            fi
            TARGET_FILE="$1"
            ;;
        --file=*)
            TARGET_FILE="${1#--file=}"
            ;;
        --host-smoke-file)
            shift
            if [ "$#" -eq 0 ] || [ -z "${1:-}" ]; then
                echo "ERROR: --host-smoke-file requires a path" >&2
                exit 2
            fi
            TARGET_HOST_SMOKE_FILE="$1"
            ;;
        --host-smoke-file=*)
            TARGET_HOST_SMOKE_FILE="${1#--host-smoke-file=}"
            ;;
        *)
            PASSTHROUGH_ARGS+=("$1")
            ;;
    esac
    shift
done

if [ -n "$TARGET_FILE" ] && [ -n "$TARGET_HOST_SMOKE_FILE" ]; then
    echo "ERROR: use either --file or --host-smoke-file, not both" >&2
    exit 2
fi

COMPONENT_SHAPE="${HOMEBOY_COMPONENT_SHAPE:-}"
if [ -z "$COMPONENT_SHAPE" ]; then
    DETECT_COMPONENT_HELPER="${HOMEBOY_RUNTIME_DETECT_COMPONENT:-${SCRIPT_DIR}/../lib/detect-component.sh}"
    # shellcheck source=../lib/detect-component.sh
    source "${DETECT_COMPONENT_HELPER}"
    if homeboy_detect_component "${COMPONENT_PATH:-$(pwd)}"; then
        COMPONENT_SHAPE="$HOMEBOY_COMPONENT_TYPE"
    fi
fi

if [ "$COMPONENT_SHAPE" = "core-dev" ]; then
    if [ -n "${HOMEBOY_TEST_SHARD_MANIFEST:-}" ]; then
        echo "ERROR: WordPress core-dev shard replay is unsupported; refusing to ignore HOMEBOY_TEST_SHARD_MANIFEST." >&2
        exit 2
    fi
    CORE_WORDPRESS_RUNTIME_RUNNER="$(homeboy_wordpress_core_runtime_runner)" || exit $?
    if [ -n "$TARGET_FILE" ]; then
        HOMEBOY_WORDPRESS_CORE_PHPUNIT_TEST_FILE="$TARGET_FILE" exec bash "$CORE_WORDPRESS_RUNTIME_RUNNER" "${PASSTHROUGH_ARGS[@]}"
    fi
    exec bash "$CORE_WORDPRESS_RUNTIME_RUNNER" "${PASSTHROUGH_ARGS[@]}"
fi

homeboy_wordpress_rel_test_file() {
    local raw_path="$1"
    local abs_path

    if [ -z "$raw_path" ]; then
        return 1
    fi

    if [ "${raw_path#/}" != "$raw_path" ]; then
        abs_path="$raw_path"
    else
        abs_path="${PLUGIN_PATH}/${raw_path}"
    fi

    if [ ! -f "$abs_path" ] && [[ "$raw_path" == wordpress/* ]]; then
        abs_path="${PLUGIN_PATH}/${raw_path#wordpress/}"
    fi

    if [ ! -f "$abs_path" ] && [ "${raw_path#/}" != "$raw_path" ]; then
        abs_path=$(homeboy_wordpress_resolve_wp_codebox_sandbox_path "$raw_path" || true)
    fi

    if [ ! -f "$abs_path" ]; then
        return 1
    fi

    case "$abs_path" in
        "${PLUGIN_PATH}"/*)
            printf '%s\n' "${abs_path#"${PLUGIN_PATH}/"}"
            ;;
        *)
            return 1
            ;;
    esac
}

homeboy_wordpress_resolve_wp_codebox_sandbox_path() {
    local sandbox_path="$1"
    local settings_json="${HOMEBOY_SETTINGS_JSON:-}"
    [ -n "$settings_json" ] || settings_json="{}"

    if [ -e "$sandbox_path" ]; then
        printf '%s\n' "$sandbox_path"
        return 0
    fi

    printf '%s' "$settings_json" | jq -r --arg sandboxPath "$sandbox_path" '
        (.wp_codebox_phpunit_mounts // [])[]
        | select((.source // "") != "" and (.target // "") != "")
        | (.target | rtrimstr("/")) as $target
        | (.source | rtrimstr("/")) as $source
        | select($sandboxPath == $target or ($sandboxPath | startswith($target + "/")))
        | $source + ($sandboxPath | sub("^" + ($target | gsub("([][\\.^$*+?{}|()-])"; "\\\\\\1")); ""))
    ' 2>/dev/null | while IFS= read -r candidate; do
        if [ -e "$candidate" ]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done
}

homeboy_wordpress_configured_phpunit_test_root() {
    local settings_json="${HOMEBOY_SETTINGS_JSON:-}"
    local test_root
    [ -n "$settings_json" ] || settings_json="{}"

    test_root=$(printf '%s' "$settings_json" | jq -r '.wp_codebox_phpunit_test_root // empty' 2>/dev/null || true)
    [ -n "$test_root" ] || return 1

    homeboy_wordpress_resolve_wp_codebox_sandbox_path "$test_root"
}

homeboy_wordpress_is_configured_phpunit_file() {
    local target_rel="$1"
    local test_root
    local target_abs="${PLUGIN_PATH}/${target_rel}"

    test_root=$(homeboy_wordpress_configured_phpunit_test_root || true)
    [ -n "$test_root" ] || return 1

    case "$target_abs" in
        "${test_root%/}"/*.php|"${test_root%/}"/*/*.php|"${test_root%/}"/*/*/*.php|"${test_root%/}"/*/*/*/*.php)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

homeboy_wordpress_full_suite_phpunit_root() {
    local configured_root
    local resolved_root

    configured_root="$(homeboy_setting wp_codebox_phpunit_test_root '.wp_codebox_phpunit_test_root' '')"
    if [ -z "$configured_root" ]; then
        printf '%s\n' "${PLUGIN_PATH}/tests"
        return 0
    fi

    resolved_root="$(homeboy_wordpress_resolve_wp_codebox_sandbox_path "$configured_root" || true)"
    [ -n "$resolved_root" ] || return 1
    printf '%s\n' "$resolved_root"
}

homeboy_wordpress_has_phpunit_tests() {
    local test_root="$1"
    local test_file

    [ -d "$test_root" ] || return 1
    if ! test_file="$(find "$test_root" -type f \( -name '*Test.php' -o -name 'test-*.php' \) -print -quit)"; then
        return 2
    fi
    [ -n "$test_file" ]
}

homeboy_wordpress_handle_no_phpunit_tests() {
    local policy
    policy="$(homeboy_setting phpunit_no_tests '.phpunit_no_tests' 'skipped')"

    case "$policy" in
        skip|skipped)
            echo "Skipping PHPUnit: no canonical test files were discovered."
            return 0
            ;;
        fail)
            echo "ERROR: no canonical PHPUnit test files were discovered." >&2
            return 1
            ;;
        *)
            echo "ERROR: unsupported phpunit_no_tests policy: ${policy}" >&2
            return 2
            ;;
    esac
}

homeboy_wordpress_run_js_smoke_files() {
    local smoke_files_raw="$1"
    local node_bin="${HOMEBOY_NODE_BIN:-node}"
    local smoke_files=()
    local smoke_file
    local smoke_abs
    local rel_path
    local passed=0

    while IFS= read -r smoke_file; do
        [ -n "$smoke_file" ] || continue
        if ! smoke_abs="$(homeboy_wordpress_rel_test_file "$smoke_file")"; then
            echo "ERROR: requested JS smoke file not found or outside the component: ${smoke_file}" >&2
            exit 2
        fi
        smoke_files+=("${PLUGIN_PATH}/${smoke_abs}")
    done <<< "$smoke_files_raw"

    if [ "${#smoke_files[@]}" -eq 0 ]; then
        echo "ERROR: no JS smoke files were selected" >&2
        exit 2
    fi

    echo "Running host JS smoke tests..."
    echo "  Component: ${HOMEBOY_COMPONENT_ID:-$(basename "$PLUGIN_PATH")} (${PLUGIN_PATH})"
    echo "  Backend: host-js-smoke"
    echo "  Files: ${#smoke_files[@]}"
    echo ""

    for smoke_file in "${smoke_files[@]}"; do
        rel_path="${smoke_file#"${PLUGIN_PATH}/"}"
        echo "JS_SMOKE_BEGIN:${rel_path}"
        if "$node_bin" "$smoke_file"; then
            echo "JS_SMOKE_OK:${rel_path}"
            passed=$((passed + 1))
        else
            exit_code=$?
            echo "JS_SMOKE_FAIL:${rel_path}:exit=${exit_code}"
            echo ""
            echo "JS smoke test failed: ${rel_path}"
            exit "$exit_code"
        fi
    done

    echo ""
    echo "JS_SMOKE_SUMMARY:passed=${passed} failed=0"
    echo "Host JS smoke test run complete."
}

homeboy_wordpress_is_js_smoke_file() {
    case "$1" in
        tests/*-smoke.js|tests/*/*-smoke.js|tests/*/*/*-smoke.js|tests/*/*/*/*-smoke.js|wordpress/tests/*-smoke.js|wordpress/tests/*/*-smoke.js|wordpress/tests/*/*/*-smoke.js|wordpress/tests/*/*/*/*-smoke.js)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

homeboy_wordpress_is_node_test_file() {
    case "$1" in
        *.test.js|*.test.cjs|*.test.mjs|*.test.jsx|*.test.ts|*.test.tsx)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

# A `*.test.js` suffix says nothing about which framework owns the file. A
# WordPress package that declares `wp-scripts test-unit-js` runs Jest, whose
# `describe`/`it`/`expect` globals, transforms, setup files, and test
# environment come from the package configuration. Narrowing a run to changed
# files must narrow the declared runner, never replace it.
#
# Resolution order, most explicit first:
#   1. HOMEBOY_WORDPRESS_JS_TEST_SCRIPT
#   2. settings wordpress_js_test_script / js_test_script
#   3. the first declared package script from
#      HOMEBOY_WORDPRESS_JS_TEST_SCRIPT_CANDIDATES
#
homeboy_wordpress_js_package_owner() {
    local rel_path="$1"
    local current="${PLUGIN_PATH}/$(dirname "$rel_path")"
    local component_root="${PLUGIN_PATH%/}"

    while :; do
        if [ -f "${current}/package.json" ]; then
            printf '%s\n' "$current"
            return 0
        fi
        [ "$current" != "$component_root" ] || break
        current="$(dirname "$current")"
    done
    printf '%s\n' "$component_root"
}

# Sets JS_TEST_SCRIPT and JS_TEST_SCRIPT_SOURCE for the package in
# HOMEBOY_PROJECT_ROOT. Returns 0 when a runner is declared, 1 when none is
# declared, and 2 when an explicitly declared script or package contract is
# missing.
homeboy_wordpress_resolve_js_test_script() {
    local package_root="$1"
    local configured candidate package_rel

    JS_TEST_SCRIPT=""
    JS_TEST_SCRIPT_SOURCE=""

    configured="${HOMEBOY_WORDPRESS_JS_TEST_SCRIPT:-}"
    if [ -z "$configured" ]; then
        configured="$(homeboy_setting wordpress_js_test_script '.wordpress_js_test_script // .js_test_script // empty')"
    fi

    if [ ! -f "${package_root}/package.json" ] || ! type homeboy_project_init >/dev/null 2>&1; then
        if [ -n "$configured" ]; then
            echo "ERROR: declared JavaScript test script '${configured}' requires an owning package manifest at ${package_root}/package.json" >&2
            return 2
        fi
        return 1
    fi

    if ! homeboy_project_init --ecosystem nodejs --path "$package_root" >/dev/null 2>&1; then
        if [ -n "$configured" ]; then
            echo "ERROR: could not resolve the Node.js project contract for owning package ${package_root}" >&2
            return 2
        fi
        return 1
    fi

    if [ -n "$configured" ]; then
        if ! homeboy_project_has_script "$configured"; then
            echo "ERROR: declared JavaScript test script '${configured}' is not defined in owning package ${package_root}/package.json" >&2
            return 2
        fi
        JS_TEST_SCRIPT="$configured"
        package_rel="${package_root#"${PLUGIN_PATH%/}/"}"
        [ "$package_rel" != "$package_root" ] || package_rel=""
        JS_TEST_SCRIPT_SOURCE="${package_rel:+${package_rel}/}package.json scripts.${configured}"
        return 0
    fi

    for candidate in ${HOMEBOY_WORDPRESS_JS_TEST_SCRIPT_CANDIDATES:-test:unit test:unit:js test:js test}; do
        if homeboy_project_has_script "$candidate"; then
            JS_TEST_SCRIPT="$candidate"
            package_rel="${package_root#"${PLUGIN_PATH%/}/"}"
            [ "$package_rel" != "$package_root" ] || package_rel=""
            JS_TEST_SCRIPT_SOURCE="${package_rel:+${package_rel}/}package.json scripts.${candidate}"
            return 0
        fi
    done

    return 1
}

# True when the file itself imports Node's built-in test runner, which is the
# only extension-independent evidence that `node --test` is the right backend.
homeboy_wordpress_is_native_node_test_file() {
    local rel_path="$1"
    local test_path="$rel_path"
    if [ "${rel_path#/}" = "$rel_path" ]; then
        test_path="${PLUGIN_PATH}/${rel_path}"
    fi
    grep -qE "(require\(|from[[:space:]]+|import[[:space:]]*\()[[:space:]]*['\"]node:test['\"]" "$test_path" 2>/dev/null
}

homeboy_wordpress_run_declared_js_test_files() {
    local package_root="$1"
    local test_files_raw="$2"
    local test_file rel_path run_command exit_code
    local selected=()

    while IFS= read -r test_file; do
        [ -n "$test_file" ] || continue
        selected+=("$test_file")
    done <<< "$test_files_raw"

    if [ "${#selected[@]}" -eq 0 ]; then
        echo "ERROR: no JavaScript test files were selected" >&2
        return 2
    fi

    if ! run_command="$(homeboy_project_run_script_command "$JS_TEST_SCRIPT")"; then
        echo "ERROR: could not resolve the package runner command for script '${JS_TEST_SCRIPT}' in owning package ${package_root}" >&2
        return 2
    fi

    if ! homeboy_project_ensure_dependencies; then
        echo "ERROR: could not hydrate dependencies for owning package ${package_root}; install its lockfile dependencies before running '${JS_TEST_SCRIPT}'" >&2
        return 2
    fi

    echo "Running declared JavaScript tests..."
    echo "  Component: ${HOMEBOY_COMPONENT_ID:-$(basename "$PLUGIN_PATH")} (${PLUGIN_PATH})"
    echo "  Package: ${package_root}"
    echo "  Backend: package-script"
    echo "  Contract: ${JS_TEST_SCRIPT_SOURCE}"
    echo "  Command: ${run_command} -- ${selected[*]}"
    for rel_path in "${selected[@]}"; do
        echo "JS_TEST_BEGIN:${rel_path}"
    done

    # Declared runners report their terminal counts in human output, and Jest
    # and wp-scripts write that summary to stderr. Tee both streams so the run
    # stays visible in logs while a copy remains parseable for structured
    # counts. Without those counts the phase reports zero executed tests and a
    # passing JavaScript-only scope is graded as a failure (#2778).
    local capture
    capture="$(mktemp "${TMPDIR:-/tmp}/homeboy-js-test.XXXXXX")"

    exit_code=0
    # Jest and wp-scripts write their summary to stderr, so both streams are
    # merged into one pipe. A `tee` into process substitution would race the
    # read below, so this pipes through `tee` and recovers the runner's real
    # status from PIPESTATUS instead of tee's.
    # shellcheck disable=SC2086
    (cd "$package_root" && $run_command -- "${selected[@]}") 2>&1 | tee -a "$capture"
    exit_code="${PIPESTATUS[0]}"

    if [ "$exit_code" -eq 0 ]; then
        echo "JS_TEST_SUMMARY:backend=package-script script=${JS_TEST_SCRIPT} files=${#selected[@]} status=ok"
    else
        echo "JS_TEST_SUMMARY:backend=package-script script=${JS_TEST_SCRIPT} files=${#selected[@]} status=failed exit=${exit_code}"
    fi

    homeboy_wordpress_write_declared_js_results "$capture" "$exit_code" "${#selected[@]}"
    rm -f "$capture"
    return "$exit_code"
}

# Normalize declared-script JavaScript results into structured counts.
#
# The generic parser resolves counts from adapters or a `passed=N failed=N`
# summary line. JS_TEST_SUMMARY carries neither, so a passing run parsed to
# zero counts and the phase failed with no failing test (#2778). Prefer exact
# counts from the runner's own summary; fall back to file-granularity only when
# the declared runner reports nothing parseable.
homeboy_wordpress_write_declared_js_results() {
    local capture="$1"
    local exit_code="$2"
    local file_count="$3"

    if ! type homeboy_write_test_results >/dev/null 2>&1 \
        && [ -n "${HOMEBOY_RUNTIME_WRITE_TEST_RESULTS:-}" ] \
        && [ -f "$HOMEBOY_RUNTIME_WRITE_TEST_RESULTS" ]; then
        # shellcheck source=/dev/null
        source "$HOMEBOY_RUNTIME_WRITE_TEST_RESULTS"
    fi
    type homeboy_write_test_results >/dev/null 2>&1 || return 0

    local adapters_helper="${HOMEBOY_RUNTIME_TEST_RESULT_ADAPTERS:-${SHARED_LIB_DIR}/test-result-adapters.sh}"
    if ! type homeboy_parse_test_results_with_adapters >/dev/null 2>&1 \
        && [ -f "$adapters_helper" ]; then
        # shellcheck source=/dev/null
        source "$adapters_helper"
    fi

    local marker="${HOMEBOY_TEST_RESULTS_FILE:-}"
    if type homeboy_parse_test_results_with_adapters >/dev/null 2>&1; then
        local stamp=""
        if [ -n "$marker" ] && [ -f "$marker" ]; then
            stamp="$(cat "$marker" 2>/dev/null || true)"
        fi
        homeboy_parse_test_results_with_adapters "$capture" jest node-test
        # The adapters only write when a runner summary parsed. A changed
        # results file means exact counts landed and no fallback is needed.
        if [ -n "$marker" ] && [ -f "$marker" ]; then
            local after=""
            after="$(cat "$marker" 2>/dev/null || true)"
            if [ "$after" != "$stamp" ]; then
                return 0
            fi
        fi
    fi

    # No parseable runner summary. Report file granularity so the phase is
    # graded on something real: a clean run is N passing files, and a failing
    # run must never be recorded as fully passing.
    if [ "$exit_code" -eq 0 ]; then
        homeboy_write_test_results "$file_count" "$file_count" 0 0 "declared-js-files"
    else
        homeboy_write_test_results "$file_count" 0 "$file_count" 0 "declared-js-files-failure"
    fi
}

# Route selected JavaScript test files to the framework that actually owns
# them: the repository's declared runner when one exists, Node's built-in
# runner when the files themselves declare it, and an actionable error when
# neither is knowable.
homeboy_wordpress_run_js_unit_test_files() {
    local test_files_raw="$1"
    local test_file rel_path package_root package_files resolve_status
    local component_root package_rel
    local ambiguous=()
    local owner_list=()
    local owner_key
    local status=0
    declare -A owner_files
    declare -A seen_owner

    component_root="${PLUGIN_PATH%/}"

    while IFS= read -r test_file; do
        [ -n "$test_file" ] || continue
        if ! rel_path="$(homeboy_wordpress_rel_test_file "$test_file")"; then
            echo "ERROR: requested JavaScript test file not found or outside the component: ${test_file}" >&2
            return 2
        fi
        package_root="$(homeboy_wordpress_js_package_owner "$rel_path")"
        package_rel="${package_root#"${component_root}/"}"
        if [ "$package_root" = "$component_root" ] || [ "$package_rel" = "$package_root" ]; then
            package_files="$rel_path"
        else
            package_files="${rel_path#"${package_rel}/"}"
        fi
        owner_files["$package_root"]+="${owner_files[$package_root]:+$'\n'}${package_files}"
        if [ -z "${seen_owner[$package_root]:-}" ]; then
            owner_list+=("$package_root")
            seen_owner["$package_root"]=1
        fi
    done <<< "$test_files_raw"

    for owner_key in "${owner_list[@]}"; do
        package_files="${owner_files[$owner_key]}"
        JS_TEST_SCRIPT=""
        JS_TEST_SCRIPT_SOURCE=""
        if homeboy_wordpress_resolve_js_test_script "$owner_key"; then
            homeboy_wordpress_run_declared_js_test_files "$owner_key" "$package_files" || status=$?
        else
            resolve_status=$?
            if [ "$resolve_status" -eq 2 ]; then
                status=2
                continue
            fi
            ambiguous=()
            while IFS= read -r rel_path; do
                [ -n "$rel_path" ] || continue
                if ! homeboy_wordpress_is_native_node_test_file "${owner_key}/${rel_path}"; then
                    ambiguous+=("$rel_path")
                fi
            done <<< "$package_files"
            if [ "${#ambiguous[@]}" -gt 0 ]; then
                echo "ERROR: cannot select a JavaScript test framework for: ${ambiguous[*]} (owning package ${owner_key})" >&2
                echo "  The files do not import Node's built-in 'node:test' runner and owning package ${owner_key}/package.json declares no JavaScript test script." >&2
                echo "  Add a package.json test script (for example \"test:unit\": \"wp-scripts test-unit-js\"), set the wordpress_js_test_script setting, or import 'node:test' in the tests." >&2
                status=2
                continue
            fi
            homeboy_wordpress_run_node_test_files "$owner_key" "$package_files" || status=$?
        fi
    done
    return "$status"
}

homeboy_wordpress_run_node_test_files() {
    local package_root="$1"
    local test_files_raw="$2"
    local node_bin="${HOMEBOY_NODE_BIN:-node}"
    local test_file test_abs rel_path exit_code
    local passed=0
    local failed=0
    local last_failure_exit=0

    echo "Running Node test files..."
    echo "  Component: ${HOMEBOY_COMPONENT_ID:-$(basename "$PLUGIN_PATH")} (${PLUGIN_PATH})"
    echo "  Package: ${package_root}"
    echo "  Backend: node-test"
    echo "  Contract: ${JS_TEST_SCRIPT_SOURCE:-native node:test import}"

    while IFS= read -r test_file; do
        [ -n "$test_file" ] || continue
        rel_path="$test_file"
        test_abs="${package_root}/${rel_path}"
        echo "NODE_TEST_BEGIN:${rel_path}"
        if (cd "$package_root" && "$node_bin" --test "$test_abs"); then
            echo "NODE_TEST_OK:${rel_path}"
            passed=$((passed + 1))
        else
            exit_code=$?
            echo "NODE_TEST_FAIL:${rel_path}:exit=${exit_code}"
            failed=$((failed + 1))
            last_failure_exit="$exit_code"
        fi
    done <<< "$test_files_raw"

    echo "NODE_TEST_SUMMARY:passed=${passed} failed=${failed}"
    [ "$failed" -eq 0 ] || return "$last_failure_exit"
}

homeboy_wordpress_run_shell_smoke_files() {
    local smoke_files_raw="$1"
    local smoke_files=()
    local smoke_file smoke_abs rel_path
    local passed=0

    while IFS= read -r smoke_file; do
        [ -n "$smoke_file" ] || continue
        if ! smoke_abs="$(homeboy_wordpress_rel_test_file "$smoke_file")"; then
            echo "ERROR: requested shell smoke file not found or outside the component: ${smoke_file}" >&2
            exit 2
        fi
        smoke_files+=("${PLUGIN_PATH}/${smoke_abs}")
    done <<< "$smoke_files_raw"

    if [ "${#smoke_files[@]}" -eq 0 ]; then
        echo "ERROR: no shell smoke files were selected" >&2
        exit 2
    fi

    echo "Running host shell smoke tests..."
    echo "  Component: ${HOMEBOY_COMPONENT_ID:-$(basename "$PLUGIN_PATH")} (${PLUGIN_PATH})"
    echo "  Backend: host-shell-smoke"
    echo "  Files: ${#smoke_files[@]}"
    echo ""

    for smoke_file in "${smoke_files[@]}"; do
        rel_path="${smoke_file#"${PLUGIN_PATH}/"}"
        echo "SHELL_SMOKE_BEGIN:${rel_path}"
        if bash "$smoke_file"; then
            echo "SHELL_SMOKE_OK:${rel_path}"
            passed=$((passed + 1))
        else
            exit_code=$?
            echo "SHELL_SMOKE_FAIL:${rel_path}:exit=${exit_code}"
            echo ""
            echo "Shell smoke test failed: ${rel_path}"
            exit "$exit_code"
        fi
    done

    echo ""
    echo "SHELL_SMOKE_SUMMARY:passed=${passed} failed=0"
    echo "Host shell smoke test run complete."
}

homeboy_wordpress_is_shell_smoke_file() {
    case "$1" in
        tests/*-smoke.sh|tests/*/*-smoke.sh|tests/*/*/*-smoke.sh|tests/*/*/*/*-smoke.sh|wordpress/tests/*-smoke.sh|wordpress/tests/*/*-smoke.sh|wordpress/tests/*/*/*-smoke.sh|wordpress/tests/*/*/*/*-smoke.sh)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

if [ -n "$TARGET_HOST_SMOKE_FILE" ]; then
    if ! target_rel="$(homeboy_wordpress_rel_test_file "$TARGET_HOST_SMOKE_FILE")"; then
        echo "ERROR: requested real-WordPress host smoke file not found: ${TARGET_HOST_SMOKE_FILE}" >&2
        exit 2
    fi

    case "$target_rel" in
        tests/*-smoke.php|tests/*/*-smoke.php|tests/*/*/*-smoke.php|tests/*/*/*/*-smoke.php)
            HOMEBOY_WORDPRESS_HOST_SMOKE_FILE="$target_rel" exec bash "$SMOKE_RUNNER" "${PASSTHROUGH_ARGS[@]}"
            ;;
        *)
            echo "ERROR: --host-smoke-file requires tests/**/*-smoke.php, got: ${target_rel}" >&2
            exit 2
            ;;
    esac
fi

if [ -z "$TARGET_FILE" ] && [ "${HOMEBOY_TEST_SCOPE_KIND:-}" = "exclusive_env" ]; then
    if [ -n "${HOMEBOY_TEST_SHARD_MANIFEST:-}" ]; then
        echo "ERROR: HOMEBOY_TEST_SHARD_MANIFEST is mutually exclusive with HOMEBOY_TEST_SCOPE_KIND=exclusive_env." >&2
        exit 2
    fi
    if [ "${HOMEBOY_TEST_SCOPE_ENV_NAME:-}" = "HOMEBOY_WORDPRESS_HOST_SMOKE_FILES" ] && [ -n "${HOMEBOY_TEST_SCOPE_ENV_VALUE:-}" ]; then
        standalone_php_smoke_files=""
        wordpress_smoke_files=""
        while IFS= read -r scoped_smoke_file; do
            [ -n "$scoped_smoke_file" ] || continue
            if ! scoped_smoke_rel="$(homeboy_wordpress_rel_test_file "$scoped_smoke_file")"; then
                echo "ERROR: requested PHP smoke file not found or outside the component: ${scoped_smoke_file}" >&2
                exit 2
            fi
            scoped_environment="$(homeboy_wordpress_test_environment "$scoped_smoke_rel")" || exit $?
            if [ "$scoped_environment" = "standalone-php" ]; then
                standalone_php_smoke_files+="${standalone_php_smoke_files:+$'\n'}${scoped_smoke_rel}"
            else
                wordpress_smoke_files+="${wordpress_smoke_files:+$'\n'}${scoped_smoke_rel}"
            fi
        done <<< "$HOMEBOY_TEST_SCOPE_ENV_VALUE"

        scope_status=0
        if [ -n "$standalone_php_smoke_files" ]; then
            homeboy_wordpress_run_standalone_php_smoke_files "$standalone_php_smoke_files" || scope_status=$?
        fi
        if [ -n "$wordpress_smoke_files" ]; then
            HOMEBOY_WORDPRESS_HOST_SMOKE_FILES="$wordpress_smoke_files" bash "$SMOKE_RUNNER" "${PASSTHROUGH_ARGS[@]}" || scope_status=$?
        fi
        exit "$scope_status"
    fi
fi

homeboy_wordpress_is_phpunit_test_file() {
    case "$(basename "$1")" in
        *Test.php|test-*.php)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

# Standalone PHP smoke scripts are host smokes with their own runners, matching
# the shape --host-smoke-file already accepts. They are not PHPUnit classes, so
# the PHPUnit matcher above rejects them; without this matcher they reach the
# changed-scope router's terminal `else` and are counted but never executed.
homeboy_wordpress_is_php_smoke_file() {
    case "$1" in
        tests/*-smoke.php|tests/*/*-smoke.php|tests/*/*/*-smoke.php|tests/*/*/*/*-smoke.php|wordpress/tests/*-smoke.php|wordpress/tests/*/*-smoke.php|wordpress/tests/*/*/*-smoke.php|wordpress/tests/*/*/*/*-smoke.php)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

# Components that use executable PHP scripts without the shared `*-smoke.php`
# convention declare their component-relative paths or shell-glob patterns in
# standalone_php_test_paths. The declaration classifies already-selected files;
# it never broadens a changed scope or turns support files into tests by default.
homeboy_wordpress_is_declared_standalone_php_test_file() {
    local test_file="$1"
    local declared_path
    local declared_paths

    case "$test_file" in
        *.php)
            ;;
        *)
            return 1
            ;;
    esac

    if [ -z "${HOMEBOY_WORDPRESS_STANDALONE_PHP_TEST_PATHS_LOADED+x}" ]; then
        declared_paths="$(homeboy_setting standalone_php_test_paths '.standalone_php_test_paths // [] | if type == "array" and all(.[]; type == "string") then .[] else error("expected an array of strings") end')" || {
            echo "ERROR: standalone_php_test_paths must be an array of strings." >&2
            return 2
        }
        HOMEBOY_WORDPRESS_STANDALONE_PHP_TEST_PATHS="$declared_paths"
        HOMEBOY_WORDPRESS_STANDALONE_PHP_TEST_PATHS_LOADED=1
    fi
    declared_paths="$HOMEBOY_WORDPRESS_STANDALONE_PHP_TEST_PATHS"
    while IFS= read -r declared_path; do
        [ -n "$declared_path" ] || continue
        case "$declared_path" in
            /*|..|../*|*/../*)
                echo "ERROR: standalone_php_test_paths contains an invalid component-relative selector: ${declared_path}" >&2
                return 2
                ;;
        esac
        if [[ "$test_file" == $declared_path ]]; then
            return 0
        fi
    done <<< "$declared_paths"
    return 1
}

homeboy_wordpress_load_test_shard_manifest() {
    local manifest_path="${HOMEBOY_TEST_SHARD_MANIFEST:-}"
    local shard_id shard_tests test_file test_rel selected_count
    local current_inventory current_runner current_workspace shard_canonical shard_fingerprint

    [ -n "$manifest_path" ] || return 0

    if [ -n "${HOMEBOY_CHANGED_TEST_FILES:-}" ] || [ -n "$TARGET_FILE" ] || [ -n "$TARGET_HOST_SMOKE_FILE" ] || [ "${#PASSTHROUGH_ARGS[@]}" -gt 0 ]; then
        echo "ERROR: HOMEBOY_TEST_SHARD_MANIFEST is mutually exclusive with other test selectors and passthrough arguments." >&2
        return 2
    fi
    if [ ! -f "$manifest_path" ] || [ -L "$manifest_path" ]; then
        echo "ERROR: WordPress test shard manifest must be a readable non-symlink file: ${manifest_path}" >&2
        return 2
    fi

    shard_id="$(jq -er '
        def hex_digest: type == "string" and test("^[0-9a-f]{64}$");
        def valid_test_path:
            type == "string"
            and length > 0
            and (startswith("/") | not)
            and (contains("\\") | not)
            and (split("/") | index("..") | not)
            and (test("[[:cntrl:]]") | not);
        if type != "object" then error("manifest must be an object")
        elif .schema != "homeboy/test-shard-manifest/v1" then error("unsupported schema")
        elif (.id | type) != "string" or (.id | test("^shard-[1-9][0-9]*$") | not) then error("invalid shard id")
        elif .runner != "wordpress" then error("runner must be wordpress")
        elif (.runner_fingerprint | hex_digest | not) then error("invalid runner fingerprint")
        elif (.workspace_fingerprint | hex_digest | not) then error("invalid workspace fingerprint")
        elif (.inventory_fingerprint | hex_digest | not) then error("invalid inventory fingerprint")
        elif (.tests | type) != "array" or (.tests | length) == 0 then error("tests must be a non-empty array")
        elif (.tests | all(valid_test_path) | not) then error("tests contain an invalid path")
        elif (.tests | unique | length) != (.tests | length) then error("tests contain duplicate paths")
        else .id end
    ' "$manifest_path")" || {
        echo "ERROR: invalid WordPress test shard manifest: ${manifest_path}" >&2
        return 2
    }
    shard_tests="$(jq -er '.tests[]' "$manifest_path")" || {
        echo "ERROR: could not read assigned tests from WordPress shard ${shard_id}." >&2
        return 2
    }

    current_inventory="$(mktemp)" || return 1
    local current_discovery
    current_discovery="$(mktemp)" || {
        rm -f "$current_inventory"
        return 1
    }
    if ! homeboy_wordpress_discover_phpunit_files "$current_discovery"; then
        rm -f "$current_inventory" "$current_discovery"
        return 2
    fi
    if ! python3 "${SCRIPT_DIR}/test-inventory.py" \
        --project "$PLUGIN_PATH" \
        --extension-path "$EXTENSION_PATH" \
        --runner wordpress \
        --package "${HOMEBOY_COMPONENT_ID:-wordpress}" \
        --discovery-file "$current_discovery" \
        --output "$current_inventory" >/dev/null; then
        rm -f "$current_inventory" "$current_discovery"
        echo "ERROR: could not regenerate the current WordPress test inventory for shard validation." >&2
        return 2
    fi
    rm -f "$current_discovery"

    current_runner="$(jq -r '.runner_fingerprint' "$current_inventory")"
    current_workspace="$(jq -r '.workspace_fingerprint' "$current_inventory")"
    if [ "$current_runner" != "$(jq -r '.runner_fingerprint' "$manifest_path")" ] \
        || [ "$current_workspace" != "$(jq -r '.workspace_fingerprint' "$manifest_path")" ]; then
        rm -f "$current_inventory"
        echo "ERROR: WordPress shard ${shard_id} is stale for the current runner or workspace." >&2
        return 2
    fi
    if ! jq -e --slurpfile manifest "$manifest_path" '
        (.tests | map(.id)) as $inventory_ids
        | all($manifest[0].tests[]; . as $id | $inventory_ids | index($id) != null)
    ' "$current_inventory" >/dev/null; then
        rm -f "$current_inventory"
        echo "ERROR: WordPress shard ${shard_id} contains a test outside the current inventory." >&2
        return 2
    fi
    shard_canonical="$(jq -cS --slurpfile manifest "$manifest_path" '
        ($manifest[0].tests | reduce .[] as $test_id ({}; .[$test_id] = true)) as $selected
        | {schema,runner,runner_fingerprint,workspace_fingerprint,tests:(.tests | map(select($selected[.id])) | sort_by(.id))}
    ' "$current_inventory")"
    rm -f "$current_inventory"
    if command -v sha256sum >/dev/null 2>&1; then
        shard_fingerprint="$(printf '%s' "$shard_canonical" | sha256sum | cut -d ' ' -f 1)"
    elif command -v shasum >/dev/null 2>&1; then
        shard_fingerprint="$(printf '%s' "$shard_canonical" | shasum -a 256 | cut -d ' ' -f 1)"
    else
        echo "ERROR: WordPress shard validation requires sha256sum or shasum." >&2
        return 2
    fi
    if [ "$shard_fingerprint" != "$(jq -r '.inventory_fingerprint' "$manifest_path")" ]; then
        echo "ERROR: WordPress shard ${shard_id} inventory fingerprint does not match its assigned tests." >&2
        return 2
    fi

    selected_count=0
    while IFS= read -r test_file; do
        [ -n "$test_file" ] || continue
        selected_count=$((selected_count + 1))
        if ! test_rel="$(homeboy_wordpress_rel_test_file "$test_file")"; then
            echo "ERROR: WordPress shard ${shard_id} assigned a missing or out-of-component test: ${test_file}" >&2
            return 2
        fi
    done <<< "$shard_tests"

    [ "$selected_count" -gt 0 ] || {
        echo "ERROR: WordPress shard ${shard_id} selected no tests." >&2
        return 2
    }

    export HOMEBOY_WORDPRESS_SHARD_TEST_FILES="$shard_tests"
    export HOMEBOY_WORDPRESS_SHARD_ID="$shard_id"
    echo "TEST_SHARD_MANIFEST:id=${shard_id} selected=${selected_count}"
}

# Dispatch host PHP smokes exactly the way the exclusive_env scope above does:
# the manifest decides which need a booted WordPress and which are standalone.
homeboy_wordpress_run_php_smoke_files() {
    local smoke_files_raw="$1"
    local smoke_rel smoke_environment declared_status
    local standalone_php_smoke_files=""
    local wordpress_smoke_files=""
    local status=0

    WORDPRESS_CHANGED_SCOPE_HOST_PHP_FILES=0

    while IFS= read -r smoke_rel; do
        [ -n "$smoke_rel" ] || continue
        # A component declaration is the explicit standalone contract. Existing
        # smoke files without that declaration retain their manifest-selected
        # WordPress or standalone environment.
        if homeboy_wordpress_is_declared_standalone_php_test_file "$smoke_rel"; then
            smoke_environment="standalone-php"
        else
            declared_status=$?
            [ "$declared_status" -eq 1 ] || return "$declared_status"
            smoke_environment="$(homeboy_wordpress_test_environment "$smoke_rel")" || return $?
        fi
        if [ "$smoke_environment" = "standalone-php" ]; then
            standalone_php_smoke_files+="${standalone_php_smoke_files:+$'\n'}${smoke_rel}"
        else
            wordpress_smoke_files+="${wordpress_smoke_files:+$'\n'}${smoke_rel}"
        fi
    done <<< "$smoke_files_raw"

    if [ -n "$standalone_php_smoke_files" ]; then
        homeboy_wordpress_run_standalone_php_smoke_files "$standalone_php_smoke_files" || status=$?
    fi
    if [ -n "$wordpress_smoke_files" ]; then
        while IFS= read -r smoke_rel; do
            [ -n "$smoke_rel" ] || continue
            WORDPRESS_CHANGED_SCOPE_HOST_PHP_FILES=$((WORDPRESS_CHANGED_SCOPE_HOST_PHP_FILES + 1))
        done <<< "$wordpress_smoke_files"
        HOMEBOY_WORDPRESS_HOST_SMOKE_FILES="$wordpress_smoke_files" bash "$SMOKE_RUNNER" "${PASSTHROUGH_ARGS[@]}" || status=$?
    fi
    return "$status"
}

homeboy_wordpress_write_host_php_results() {
    local total="$1"
    local passed="$2"
    local failed="$3"
    local source="$4"

    if ! type homeboy_write_test_results >/dev/null 2>&1 && [ -n "${HOMEBOY_RUNTIME_WRITE_TEST_RESULTS:-}" ] && [ -f "$HOMEBOY_RUNTIME_WRITE_TEST_RESULTS" ]; then
        # shellcheck source=/dev/null
        source "$HOMEBOY_RUNTIME_WRITE_TEST_RESULTS"
    fi
    if type homeboy_write_test_results >/dev/null 2>&1; then
        homeboy_write_test_results "$total" "$passed" "$failed" 0 "$source"
    elif [ -n "${HOMEBOY_TEST_RESULTS_FILE:-}" ]; then
        echo "ERROR: WordPress host-PHP result normalization cannot write HOMEBOY_TEST_RESULTS_FILE." >&2
        return 2
    fi
}

homeboy_wordpress_collect_full_suite_standalone_php_files() {
    local test_file test_rel declared_status
    local selected=0
    local routed=0
    local excluded=0

    FULL_SUITE_STANDALONE_PHP_FILES=""
    FULL_SUITE_STANDALONE_PHP_SELECTED=0
    FULL_SUITE_STANDALONE_PHP_ROUTED=0
    FULL_SUITE_STANDALONE_PHP_EXCLUDED=0

    while IFS= read -r test_file; do
        [ -n "$test_file" ] || continue
        test_rel="${test_file#"${PLUGIN_PATH}/"}"
        if homeboy_wordpress_is_declared_standalone_php_test_file "$test_rel"; then
            selected=$((selected + 1))
            FULL_SUITE_STANDALONE_PHP_FILES+="${FULL_SUITE_STANDALONE_PHP_FILES:+$'\n'}${test_rel}"
            routed=$((routed + 1))
            echo "FULL_SUITE_STANDALONE_PHP_ROUTE:${test_rel}:runner=host-php-smoke"
        else
            declared_status=$?
            [ "$declared_status" -eq 1 ] || return "$declared_status"
            # Only declared files belong to this standalone suite. Everything
            # else retains its existing PHPUnit/support-file classification.
            excluded=$((excluded + 1))
        fi
    done < <(find "$PLUGIN_PATH" -type f -name '*.php' -print | sort)

    FULL_SUITE_STANDALONE_PHP_SELECTED="$selected"
    FULL_SUITE_STANDALONE_PHP_ROUTED="$routed"
    FULL_SUITE_STANDALONE_PHP_EXCLUDED="$excluded"
}

homeboy_wordpress_replay_test_shard() {
    local test_file test_rel
    local phpunit_files=""
    local selected=0
    local routed=0

    while IFS= read -r test_file; do
        [ -n "$test_file" ] || continue
        selected=$((selected + 1))
        test_rel="$(homeboy_wordpress_rel_test_file "$test_file")" || return 2
        phpunit_files+="${phpunit_files:+$'\n'}${test_rel}"
        echo "TEST_SHARD_ROUTE:${test_rel}:runner=phpunit"
        routed=$((routed + 1))
    done <<< "$HOMEBOY_WORDPRESS_SHARD_TEST_FILES"

    if [ -n "$phpunit_files" ]; then
        export HOMEBOY_WORDPRESS_PHPUNIT_CHANGED_TEST_FILES="$phpunit_files"
        WORDPRESS_RUNTIME_RUNNER="$(homeboy_wordpress_runtime_runner)" || return $?
        bash "$WORDPRESS_RUNTIME_RUNNER" "${PASSTHROUGH_ARGS[@]}" || return $?
    fi

    [ "$selected" -eq "$routed" ] || {
        echo "ERROR: WordPress shard ${HOMEBOY_WORDPRESS_SHARD_ID} routed ${routed} of ${selected} assigned tests." >&2
        return 2
    }
    echo "TEST_SHARD_SUMMARY:id=${HOMEBOY_WORDPRESS_SHARD_ID} selected=${selected} routed=${routed} status=passed"
    if ! type homeboy_write_test_results >/dev/null 2>&1 && [ -n "${HOMEBOY_RUNTIME_WRITE_TEST_RESULTS:-}" ] && [ -f "$HOMEBOY_RUNTIME_WRITE_TEST_RESULTS" ]; then
        # shellcheck source=/dev/null
        source "$HOMEBOY_RUNTIME_WRITE_TEST_RESULTS"
    fi
    if type homeboy_write_test_results >/dev/null 2>&1; then
        homeboy_write_test_results "$selected" "$selected" 0 0 "shard-membership"
    elif [ -n "${HOMEBOY_TEST_RESULTS_FILE:-}" ]; then
        echo "ERROR: WordPress shard result normalization cannot write HOMEBOY_TEST_RESULTS_FILE." >&2
        return 2
    fi
}

if [ -n "${HOMEBOY_TEST_SHARD_MANIFEST:-}" ]; then
    homeboy_wordpress_load_test_shard_manifest || exit $?
    homeboy_wordpress_replay_test_shard || exit $?
    exit 0
fi

if [ -z "$TARGET_FILE" ] && [ -n "${HOMEBOY_CHANGED_TEST_FILES:-}" ]; then
    changed_js_smoke_files=""
    changed_shell_smoke_files=""
    changed_node_test_files=""
    changed_php_smoke_files=""
    changed_phpunit_files=""
    changed_non_host_smoke_files=0
    changed_selected_count=0
    changed_routed_count=0
    changed_excluded_count=0
    while IFS= read -r changed_test_file; do
        [ -n "$changed_test_file" ] || continue
        changed_selected_count=$((changed_selected_count + 1))
        if ! changed_test_rel="$(homeboy_wordpress_rel_test_file "$changed_test_file")"; then
            # Every selected path must be accounted for. A path that cannot be
            # resolved inside the component is a real exclusion, not a silent
            # drop, so name it and its reason.
            echo "CHANGED_SCOPE_EXCLUDED:${changed_test_file}:reason=unresolved_outside_component"
            changed_excluded_count=$((changed_excluded_count + 1))
            changed_non_host_smoke_files=1
            continue
        fi
        if homeboy_wordpress_is_js_smoke_file "$changed_test_rel"; then
            if [ -n "$changed_js_smoke_files" ]; then
                changed_js_smoke_files+=$'\n'
            fi
            changed_js_smoke_files+="$changed_test_rel"
            echo "CHANGED_SCOPE_ROUTE:${changed_test_rel}:runner=host-js-smoke"
            changed_routed_count=$((changed_routed_count + 1))
        elif homeboy_wordpress_is_shell_smoke_file "$changed_test_rel"; then
            if [ -n "$changed_shell_smoke_files" ]; then
                changed_shell_smoke_files+=$'\n'
            fi
            changed_shell_smoke_files+="$changed_test_rel"
            echo "CHANGED_SCOPE_ROUTE:${changed_test_rel}:runner=host-shell-smoke"
            changed_routed_count=$((changed_routed_count + 1))
        elif homeboy_wordpress_is_node_test_file "$changed_test_rel"; then
            if [ -n "$changed_node_test_files" ]; then
                changed_node_test_files+=$'\n'
            fi
            changed_node_test_files+="$changed_test_rel"
            echo "CHANGED_SCOPE_ROUTE:${changed_test_rel}:runner=node-test"
            changed_routed_count=$((changed_routed_count + 1))
        elif homeboy_wordpress_is_php_smoke_file "$changed_test_rel"; then
            if [ -n "$changed_php_smoke_files" ]; then
                changed_php_smoke_files+=$'\n'
            fi
            changed_php_smoke_files+="$changed_test_rel"
            echo "CHANGED_SCOPE_ROUTE:${changed_test_rel}:runner=host-php-smoke"
            changed_routed_count=$((changed_routed_count + 1))
        elif homeboy_wordpress_is_declared_standalone_php_test_file "$changed_test_rel"; then
            if [ -n "$changed_php_smoke_files" ]; then
                changed_php_smoke_files+=$'\n'
            fi
            changed_php_smoke_files+="$changed_test_rel"
            echo "CHANGED_SCOPE_ROUTE:${changed_test_rel}:runner=host-php-smoke"
            changed_routed_count=$((changed_routed_count + 1))
        elif homeboy_wordpress_is_phpunit_test_file "$changed_test_rel"; then
            if [ -n "$changed_phpunit_files" ]; then
                changed_phpunit_files+=$'\n'
            fi
            changed_phpunit_files+="$changed_test_rel"
            changed_non_host_smoke_files=1
            echo "CHANGED_SCOPE_ROUTE:${changed_test_rel}:runner=phpunit"
            changed_routed_count=$((changed_routed_count + 1))
        else
            # Support files (fixtures, test doubles) are legitimately not
            # executable tests. That is a correct exclusion, but it must be a
            # recorded classification rather than a path that just disappears.
            echo "CHANGED_SCOPE_EXCLUDED:${changed_test_rel}:reason=unsupported_test_shape"
            changed_excluded_count=$((changed_excluded_count + 1))
            changed_non_host_smoke_files=1
        fi
    done <<< "$HOMEBOY_CHANGED_TEST_FILES"

    # Executed counts must reconcile with selected files. This summary is the
    # ledger a reviewer reads when a changed scope reports zero tests.
    echo "CHANGED_SCOPE_SUMMARY:selected=${changed_selected_count} routed=${changed_routed_count} excluded=${changed_excluded_count}"

    # Selected PHPUnit files must reach the WordPress runtime backend as an
    # explicit scope. Without it a changed-file review silently widens to the
    # full suite, and the backend cannot report whether the selection ran.
    if [ -n "$changed_phpunit_files" ]; then
        export HOMEBOY_WORDPRESS_PHPUNIT_CHANGED_TEST_FILES="$changed_phpunit_files"
    fi

    # Host PHP smokes run regardless of what else is in scope, and their status
    # is carried into whichever exit path this branch takes. A failing smoke
    # must not be erased by a passing PHPUnit run that follows it.
    changed_scope_status=0
    if [ -n "$changed_php_smoke_files" ]; then
        homeboy_wordpress_run_php_smoke_files "$changed_php_smoke_files" || changed_scope_status=$?
    fi

    if [ -n "$changed_js_smoke_files" ] && [ -z "$changed_shell_smoke_files" ] && [ -z "$changed_node_test_files" ] && [ "$changed_non_host_smoke_files" -eq 0 ]; then
        homeboy_wordpress_run_js_smoke_files "$changed_js_smoke_files"
        exit "$changed_scope_status"
    fi

    if [ -z "$changed_js_smoke_files" ] && [ -n "$changed_shell_smoke_files" ] && [ -z "$changed_node_test_files" ] && [ "$changed_non_host_smoke_files" -eq 0 ]; then
        homeboy_wordpress_run_shell_smoke_files "$changed_shell_smoke_files"
        exit "$changed_scope_status"
    fi

    if [ -z "$changed_js_smoke_files" ] && [ -z "$changed_shell_smoke_files" ] && [ -n "$changed_node_test_files" ] && [ "$changed_non_host_smoke_files" -eq 0 ]; then
        homeboy_wordpress_run_js_unit_test_files "$changed_node_test_files" || changed_scope_status=$?
        exit "$changed_scope_status"
    fi

    if [ -n "$changed_node_test_files" ]; then
        homeboy_wordpress_run_js_unit_test_files "$changed_node_test_files" || exit $?
    fi

    # PHP smokes with nothing requiring the PHPUnit backend are a complete
    # scope on their own. Falling through here would hand WP Codebox an empty
    # changed-test list, which it reads as "run everything" — silently widening
    # a changed-file review to the full suite.
    if [ -n "$changed_php_smoke_files" ] && [ -z "$changed_js_smoke_files" ] && [ -z "$changed_shell_smoke_files" ] && [ -z "$changed_node_test_files" ] && [ "$changed_non_host_smoke_files" -eq 0 ]; then
        # A standalone-only scope has exact host-PHP counts. Publish them here
        # instead of falling through to PHPUnit, whose empty changed scope means
        # a full-suite run and whose results would not describe these scripts.
        if [ "${WORDPRESS_CHANGED_SCOPE_HOST_PHP_FILES:-0}" -eq 0 ]; then
            homeboy_wordpress_write_host_php_results \
                "$changed_routed_count" \
                "${WORDPRESS_STANDALONE_PHP_SMOKE_PASSED:-0}" \
                "${WORDPRESS_STANDALONE_PHP_SMOKE_FAILED:-0}" \
                "changed-scope-host-php" || changed_scope_status=$?
        fi
        exit "$changed_scope_status"
    fi
fi

if [ -n "$TARGET_FILE" ]; then
    if ! target_rel="$(homeboy_wordpress_rel_test_file "$TARGET_FILE")"; then
        echo "ERROR: requested test file not found: ${TARGET_FILE}" >&2
        exit 2
    fi

    target_base="$(basename "$target_rel")"
    if homeboy_wordpress_is_node_test_file "$target_rel"; then
        homeboy_wordpress_run_js_unit_test_files "$target_rel"
        exit $?
    fi

    if homeboy_wordpress_is_js_smoke_file "$target_rel"; then
        homeboy_wordpress_run_js_smoke_files "$target_rel"
        exit 0
    fi

    if homeboy_wordpress_is_shell_smoke_file "$target_rel"; then
        homeboy_wordpress_run_shell_smoke_files "$target_rel"
        exit 0
    fi

    if homeboy_wordpress_is_configured_phpunit_file "$target_rel"; then
        configured_phpunit_root=$(homeboy_wordpress_configured_phpunit_test_root || true)
        configured_phpunit_target="${PLUGIN_PATH}/${target_rel}"
        configured_phpunit_rel="${configured_phpunit_target#"${configured_phpunit_root%/}/"}"
        if [ -z "$configured_phpunit_root" ] || [ "$configured_phpunit_rel" = "$configured_phpunit_target" ]; then
            configured_phpunit_rel="$target_rel"
        fi
        case "$target_base" in
            *Test.php|test-*.php)
                WORDPRESS_RUNTIME_RUNNER="$(homeboy_wordpress_runtime_runner)" || exit $?
                HOMEBOY_WORDPRESS_PHPUNIT_TEST_FILE="$configured_phpunit_rel" exec bash "$WORDPRESS_RUNTIME_RUNNER" "${PASSTHROUGH_ARGS[@]}"
                ;;
            *)
                echo "ERROR: cannot classify requested WordPress test file under configured PHPUnit test root: ${target_rel}" >&2
                echo "  PHPUnit files must match *Test.php or test-*.php." >&2
                exit 2
                ;;
        esac
    fi

    case "$target_rel" in
        tests/*.php|tests/*/*.php|tests/*/*/*.php|tests/*/*/*/*.php)
            case "$target_base" in
                *-smoke.php)
                    target_environment="$(homeboy_wordpress_test_environment "$target_rel")" || exit $?
                    if [ "$target_environment" = "standalone-php" ]; then
                        # The sibling branch execs, so it terminates here. This
                        # one returns, and without an explicit exit the request
                        # for one file falls through into the full-suite PHPUnit
                        # run below.
                        homeboy_wordpress_run_standalone_php_smoke_files "$target_rel"
                        exit $?
                    else
                        HOMEBOY_WORDPRESS_HOST_SMOKE_FILE="$target_rel" exec bash "$SMOKE_RUNNER" "${PASSTHROUGH_ARGS[@]}"
                    fi
                    ;;
                *Test.php|test-*.php)
                    WORDPRESS_RUNTIME_RUNNER="$(homeboy_wordpress_runtime_runner)" || exit $?
                    HOMEBOY_WORDPRESS_PHPUNIT_TEST_FILE="$target_rel" exec bash "$WORDPRESS_RUNTIME_RUNNER" "${PASSTHROUGH_ARGS[@]}"
                    ;;
                *)
                    echo "ERROR: cannot classify requested WordPress test file: ${target_rel}" >&2
                    echo "  Standalone smoke files must match tests/**/*-smoke.php." >&2
                    echo "  PHPUnit files must match tests/**/*Test.php or tests/**/test-*.php." >&2
                    exit 2
                    ;;
            esac
            ;;
        *)
            echo "ERROR: requested WordPress test file must live under tests/: ${target_rel}" >&2
            exit 2
            ;;
    esac
fi

# Full-suite run (no --file, no changed-file scope): declared standalone PHP
# tests are part of the suite, while convention-only smoke scripts remain
# explicit diagnostic targets. The declaration uses the same classifier and
# bounded host-PHP runner as changed scopes.
homeboy_wordpress_collect_full_suite_standalone_php_files || exit $?
full_suite_phpunit_root="$(homeboy_wordpress_full_suite_phpunit_root || true)"
full_suite_phpunit_status=0
if [ -n "$full_suite_phpunit_root" ]; then
    homeboy_wordpress_has_phpunit_tests "$full_suite_phpunit_root" || full_suite_phpunit_status=$?
    if [ "$full_suite_phpunit_status" -ne 0 ] && [ "$full_suite_phpunit_status" -ne 1 ]; then
        echo "ERROR: unable to inspect PHPUnit test root: ${full_suite_phpunit_root}" >&2
        exit "$full_suite_phpunit_status"
    fi
fi

full_suite_standalone_status=0
if [ -n "$FULL_SUITE_STANDALONE_PHP_FILES" ]; then
    homeboy_wordpress_run_standalone_php_smoke_files "$FULL_SUITE_STANDALONE_PHP_FILES" || full_suite_standalone_status=$?
fi
echo "FULL_SUITE_STANDALONE_PHP_SUMMARY:candidates=$((FULL_SUITE_STANDALONE_PHP_SELECTED + FULL_SUITE_STANDALONE_PHP_EXCLUDED)) selected=${FULL_SUITE_STANDALONE_PHP_SELECTED} routed=${FULL_SUITE_STANDALONE_PHP_ROUTED} excluded=${FULL_SUITE_STANDALONE_PHP_EXCLUDED} passed=${WORDPRESS_STANDALONE_PHP_SMOKE_PASSED:-0} failed=${WORDPRESS_STANDALONE_PHP_SMOKE_FAILED:-0}"

if [ "$full_suite_phpunit_status" -eq 1 ]; then
    if [ "$FULL_SUITE_STANDALONE_PHP_SELECTED" -gt 0 ]; then
        homeboy_wordpress_write_host_php_results \
            "$FULL_SUITE_STANDALONE_PHP_SELECTED" \
            "${WORDPRESS_STANDALONE_PHP_SMOKE_PASSED:-0}" \
            "${WORDPRESS_STANDALONE_PHP_SMOKE_FAILED:-0}" \
            "full-suite-host-php" || exit $?
        exit "$full_suite_standalone_status"
    else
        homeboy_wordpress_handle_no_phpunit_tests
        exit $?
    fi
fi
WORDPRESS_RUNTIME_RUNNER="$(homeboy_wordpress_runtime_runner)" || exit $?
# A host PHP smoke in this changed scope already failed. `exec` would replace
# this process and report only the PHPUnit backend's status, erasing that
# failure. Run the backend for its evidence, then report the worst status.
if [ "${changed_scope_status:-0}" -ne 0 ] || [ "$full_suite_standalone_status" -ne 0 ]; then
    wordpress_runtime_status=0
    bash "$WORDPRESS_RUNTIME_RUNNER" "${PASSTHROUGH_ARGS[@]}" || wordpress_runtime_status=$?
    if [ "$wordpress_runtime_status" -ne 0 ]; then
        exit "$wordpress_runtime_status"
    fi
    if [ "${changed_scope_status:-0}" -ne 0 ]; then
        exit "$changed_scope_status"
    fi
    exit "$full_suite_standalone_status"
fi
exec bash "$WORDPRESS_RUNTIME_RUNNER" "${PASSTHROUGH_ARGS[@]}"
