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
RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:?HOMEBOY_RUNTIME_RESOLVE_CONTEXT is required}"
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
HOST_SMOKE_TIMEOUT_SECONDS="${HOMEBOY_WORDPRESS_HOST_SMOKE_TIMEOUT_SECONDS:-120}"
HOST_SMOKE_EXCERPT_BYTES="${HOMEBOY_WORDPRESS_HOST_SMOKE_EXCERPT_BYTES:-65536}"
WP_CODEBOX_TIMEOUT_DIAGNOSTICS="${SCRIPT_DIR}/../lib/wp-codebox-timeout-diagnostics.mjs"

case "$PLUGIN_SLUG" in
    ''|*[!A-Za-z0-9._-]*)
        echo "ERROR: component has an unsafe WordPress plugin slug: ${PLUGIN_SLUG}" >&2
        exit 2
        ;;
esac

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

homeboy_wordpress_smoke_wp_version() {
    local version=""
    if [ -n "${HOMEBOY_SETTINGS_JSON:-}" ] && [ "${HOMEBOY_SETTINGS_JSON}" != "{}" ]; then
        local extracted
        extracted=$(printf '%s' "$HOMEBOY_SETTINGS_JSON" | jq -r '.wordpress_runtime_version // .wp_codebox_wordpress_version // empty' 2>/dev/null || true)
        [ -n "$extracted" ] && [ "$extracted" != "null" ] && version="$extracted"
    fi
    printf '%s\n' "$version"
}

# Build the recipe mounts and child environment mapping together so every
# dependency root exposed to a smoke is the translated path that was mounted.
homeboy_wordpress_smoke_recipe_inputs() {
    local mounts_json='[]'
    local dependency_roots_json='{}'
    local plugin_source
    plugin_source="$(homeboy_wp_codebox_resolve_mount_path "$PLUGIN_PATH")"
    mounts_json=$(jq -nc --argjson mounts "$mounts_json" --arg source "$plugin_source" --arg target "/wordpress/wp-content/plugins/${PLUGIN_SLUG}" '$mounts + [{source: $source, target: $target, mode: "readonly"}]')

    if type homeboy_export_validation_dependency_paths >/dev/null 2>&1; then
        homeboy_export_validation_dependency_paths "$PLUGIN_PATH" >/dev/null 2>&1 || true
    fi
    if [ -n "${HOMEBOY_WORDPRESS_DEPENDENCY_PATHS:-}" ]; then
        local dep_path dep_slug dep_source dep_target
        while IFS= read -r dep_path; do
            [ -n "$dep_path" ] || continue
            [ -d "$dep_path" ] || continue
            if type homeboy_get_validation_dependency_slug >/dev/null 2>&1; then
                dep_slug="$(homeboy_get_validation_dependency_slug "$dep_path" || basename "$dep_path")"
            else
                dep_slug="$(basename "$dep_path")"
            fi
            case "$dep_slug" in
                ''|*[!A-Za-z0-9._-]*)
                    echo "ERROR: validation dependency has an unsafe WordPress plugin slug: ${dep_slug}" >&2
                    return 1
                    ;;
            esac
            dep_source="$(homeboy_wp_codebox_resolve_mount_path "$dep_path")"
            dep_target="/wordpress/wp-content/plugins/${dep_slug}"
            mounts_json=$(jq -nc --argjson mounts "$mounts_json" --arg source "$dep_source" --arg target "$dep_target" '$mounts + [{source: $source, target: $target, mode: "readonly"}]')
            dependency_roots_json=$(jq -nc --argjson roots "$dependency_roots_json" --arg slug "$dep_slug" --arg root "$dep_target" '$roots + {($slug): $root}')
        done <<< "$HOMEBOY_WORDPRESS_DEPENDENCY_PATHS"
    fi

    if [ -f "${PLUGIN_PATH}/db.php" ]; then
        local db_source
        db_source="$(homeboy_wp_codebox_resolve_mount_path "${PLUGIN_PATH}/db.php")"
        mounts_json=$(jq -nc --argjson mounts "$mounts_json" --arg source "$db_source" --arg target "/wordpress/wp-content/db.php" '$mounts + [{source: $source, target: $target, mode: "readonly"}]')
    fi

    jq -nc --argjson mounts "$mounts_json" --argjson dependencyRoots "$dependency_roots_json" '{mounts: $mounts, dependency_roots: $dependencyRoots}'
}

# Emit (to stdout) a PHP wrapper that, when executed inside the booted-WP
# sandbox via wordpress.run-php, requires the mounted smoke file and maps a
# thrown exception / fatal into a non-zero exit so the recipe records a failure.
# This prints the wrapper source; it does not run the smoke on the host.
homeboy_wordpress_smoke_wrapper() {
    local sandbox_smoke_path="$1"
    local environment_json="$2"
    php -r '
        $smoke = $argv[1];
        $environment = json_decode($argv[2], true, 512, JSON_THROW_ON_ERROR);
        echo "<?php\n";
        echo "\$smoke = " . var_export($smoke, true) . ";\n";
        echo "\$homeboy_smoke_environment = " . var_export($environment, true) . ";\n";
        echo "foreach (\$homeboy_smoke_environment as \$name => \$value) { putenv(\$name . \"=\" . \$value); \$_ENV[\$name] = \$value; }\n";
        echo "\$homeboy_smoke_stderr = defined(\"STDERR\") ? STDERR : fopen(\"php://stderr\", \"w\");\n";
        echo "if (!is_resource(\$homeboy_smoke_stderr)) { \$homeboy_smoke_stderr = fopen(\"php://output\", \"w\"); }\n";
        echo "if (!file_exists(\$smoke)) { fwrite(\$homeboy_smoke_stderr, \"smoke file missing in sandbox: \" . \$smoke . \"\\n\"); exit(3); }\n";
        echo "register_shutdown_function(function () { \$e = error_get_last(); if (\$e && in_array(\$e[\"type\"], array(E_ERROR, E_PARSE, E_CORE_ERROR, E_COMPILE_ERROR), true)) { exit(1); } });\n";
        echo "try { require \$smoke; } catch (\\Throwable \$e) { fwrite(\$homeboy_smoke_stderr, \"smoke threw: \" . \$e->getMessage() . \"\\n\"); exit(1); }\n";
    ' "$sandbox_smoke_path" "$environment_json"
}

run_one_smoke() {
    local smoke_file="$1"
    local rel_path="${smoke_file#"${PLUGIN_PATH}/"}"
    local sandbox_smoke_path="/wordpress/wp-content/plugins/${PLUGIN_SLUG}/${rel_path}"
    local wrapper_file recipe_file output_file stderr_file termination_file artifacts_dir status started_at elapsed

    artifacts_dir="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wp-smoke-artifacts.XXXXXX")"
    wrapper_file="${artifacts_dir}/wrapper.php"
    recipe_file="${artifacts_dir}/recipe.json"
    output_file="${artifacts_dir}/recipe-run.json"
    stderr_file="${artifacts_dir}/wp-codebox.stderr"
    termination_file="${artifacts_dir}/termination.json"

    homeboy_wordpress_smoke_wrapper "$sandbox_smoke_path" "$RECIPE_ENVIRONMENT" > "$wrapper_file"
    echo "HOST_SMOKE_PROGRESS:${rel_path}:phase=wrapper-created artifacts=${artifacts_dir}"

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
    echo "HOST_SMOKE_PROGRESS:${rel_path}:phase=recipe-created artifacts=${artifacts_dir}"

    set +e
    started_at="$(date +%s)"
    echo "HOST_SMOKE_PROGRESS:${rel_path}:phase=wp-codebox-recipe-run timeout=${HOST_SMOKE_TIMEOUT_SECONDS}s artifacts=${artifacts_dir}"
    homeboy_wordpress_smoke_run_with_timeout "$output_file" "$stderr_file" "$termination_file" \
        "${WP_CODEBOX_COMMAND[@]}" recipe-run --recipe "$recipe_file" --artifacts "$artifacts_dir" --json
    status=$?
    elapsed=$(( $(date +%s) - started_at ))
    set -e
    echo "HOST_SMOKE_PROGRESS:${rel_path}:phase=output-parsing exit=${status} elapsed=${elapsed}s artifacts=${artifacts_dir}"

    if [ "$status" -eq 124 ]; then
        echo "HOST_SMOKE_TIMEOUT:${rel_path}:phase=wp-codebox-recipe-run elapsed=${elapsed}s timeout=${HOST_SMOKE_TIMEOUT_SECONDS}s artifacts=${artifacts_dir}"
        node "$WP_CODEBOX_TIMEOUT_DIAGNOSTICS" wp-codebox-recipe-run "$elapsed" "$HOST_SMOKE_TIMEOUT_SECONDS" "$rel_path" "$termination_file" "$output_file" "$stderr_file" "$artifacts_dir"
        return 124
    fi

    if [ "$status" -eq 0 ] && ! jq -e '.success == true' "$output_file" >/dev/null 2>&1; then
        status=1
    fi
    if [ "$status" -ne 0 ]; then
        homeboy_wordpress_smoke_print_failure_output "$rel_path" "$output_file" "$stderr_file" "$artifacts_dir"
    else
        jq -r '(.executions // [])[-1].stdout // empty' "$output_file" 2>/dev/null || true
        rm -rf "$artifacts_dir"
    fi

    return "$status"
}

homeboy_wordpress_smoke_run_with_timeout() {
    local output_file="$1"
    local stderr_file="$2"
    local termination_file="$3"
    shift 3

    node -e '
        const fs = require("node:fs");
        const { spawn } = require("node:child_process");
        const { pathToFileURL } = require("node:url");
        const [timeoutSeconds, stdoutFile, stderrFile, terminationFile, redactorModule, ...command] = process.argv.slice(1);
        (async () => {
        const { createTimeoutLineRedactor } = await import(pathToFileURL(redactorModule));
        const stdout = fs.createWriteStream(stdoutFile);
        const stderr = fs.createWriteStream(stderrFile);
        const stdoutRedactor = createTimeoutLineRedactor();
        const stderrRedactor = createTimeoutLineRedactor();
        const child = spawn(command[0], command.slice(1), { stdio: ["ignore", "pipe", "pipe"], detached: true });
        let timedOut = false;
        let childError;
        let timeoutClosed = false;
        let killEscalated = false;
        const killGroup = (signal) => {
            try { process.kill(-child.pid, signal); }
            catch (error) { if (error.code !== "ESRCH") { try { child.kill(signal); } catch {} } }
        };
        const timer = setTimeout(() => {
            timedOut = true;
            killGroup("SIGTERM");
            // This begins at timeout, not at leader close: descendants can hold
            // inherited pipes open after their parent exits.
            setTimeout(() => {
                killEscalated = true;
                killGroup("SIGKILL");
                if (timeoutClosed) process.exit(124);
            }, 5000);
        }, Number(timeoutSeconds) * 1000);
        child.stdout.on("data", (chunk) => stdout.write(stdoutRedactor.write(chunk)));
        child.stderr.on("data", (chunk) => stderr.write(stderrRedactor.write(chunk)));
        child.on("error", (error) => { childError = error; });
        child.on("close", (code, signal) => {
            clearTimeout(timer);
            stdout.end(stdoutRedactor.end());
            stderr.end(stderrRedactor.end());
            Promise.all([new Promise((done) => stdout.on("finish", done)), new Promise((done) => stderr.on("finish", done))]).then(() => {
                fs.writeFileSync(terminationFile, JSON.stringify({ result: timedOut ? "timeout" : "exited", signal: signal || undefined, code }));
                if (timedOut) { timeoutClosed = true; if (killEscalated) process.exit(124); return; }
                if (childError) { fs.appendFileSync(stderrFile, `${childError.message}\n`); process.exit(1); }
                process.exit(code === null ? 1 : code);
            });
        });
        })().catch((error) => { fs.appendFileSync(stderrFile, `${error.message}\n`); process.exit(1); });
    ' "$HOST_SMOKE_TIMEOUT_SECONDS" "$output_file" "$stderr_file" "$termination_file" "$WP_CODEBOX_TIMEOUT_DIAGNOSTICS" "$@"
}

homeboy_wordpress_smoke_print_failure_output() {
    local rel_path="$1"
    local output_file="$2"
    local stderr_file="$3"
    local artifacts_dir="$4"
    local smoke_stdout="${artifacts_dir}/smoke.stdout.txt"
    local smoke_stderr="${artifacts_dir}/smoke.stderr.txt"

    jq -r '(.executions // [])[-1].stdout // empty' "$output_file" > "$smoke_stdout" 2>/dev/null || true
    jq -r '(.executions // [])[-1].stderr // empty' "$output_file" > "$smoke_stderr" 2>/dev/null || true

    echo "HOST_SMOKE_OUTPUT_BEGIN:${rel_path}"
    homeboy_wordpress_smoke_print_excerpt "stdout" "$smoke_stdout"
    homeboy_wordpress_smoke_print_excerpt "stderr" "$smoke_stderr"
    homeboy_wordpress_smoke_print_excerpt "wp-codebox-stderr" "$stderr_file"
    if [ ! -s "$smoke_stdout" ] && [ ! -s "$smoke_stderr" ] && [ -s "$output_file" ]; then
        homeboy_wordpress_smoke_print_excerpt "wp-codebox-json" "$output_file"
    fi
    echo "HOST_SMOKE_OUTPUT_END:${rel_path}:artifacts=${artifacts_dir}"
}

homeboy_wordpress_smoke_print_excerpt() {
    local label="$1"
    local file="$2"

    [ -s "$file" ] || return 0
    echo "--- ${label} (${file}) ---"
    python3 - "$file" "$HOST_SMOKE_EXCERPT_BYTES" <<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
limit = int(sys.argv[2])
data = path.read_bytes()
sys.stdout.buffer.write(data[:limit])
if data and not data.endswith(b"\n"):
    sys.stdout.write("\n")
if len(data) > limit:
    sys.stdout.write(f"[truncated {len(data) - limit} bytes; full output retained at {path}]\n")
PY
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
    smoke_files=()
fi

if [ "${#smoke_files[@]}" -eq 0 ]; then
    echo ""
    echo "Skipping real-WordPress smoke tests: no smoke files requested. Use --host-smoke-file or HOMEBOY_WORDPRESS_HOST_SMOKE_FILES to run one explicitly."
    exit 0
fi

homeboy_wp_codebox_export_command "${HOMEBOY_SETTINGS_JSON:-}" || exit 1
homeboy_wp_codebox_preflight_command || exit 1
WP_CODEBOX_COMMAND=("${HOMEBOY_WP_CODEBOX_COMMAND[@]}")
WP_VERSION="$(homeboy_wordpress_smoke_wp_version)"
RECIPE_INPUTS="$(homeboy_wordpress_smoke_recipe_inputs)" || exit 1
RECIPE_MOUNTS="$(jq -c '.mounts' <<< "$RECIPE_INPUTS")"
RECIPE_DEPENDENCY_ROOTS="$(jq -c '.dependency_roots' <<< "$RECIPE_INPUTS")"
RECIPE_ENVIRONMENT="$(jq -c '
    def namespaced_dependency_name:
        split("")
        | map(
            if test("^[a-z]$") then "LOWER_" + ascii_upcase
            elif test("^[A-Z]$") then "UPPER_" + .
            elif test("^[0-9]$") then "DIGIT_" + .
            elif . == "-" then "HYPHEN"
            elif . == "." then "DOT"
            elif . == "_" then "UNDERSCORE"
            else error("unsupported WordPress plugin slug character")
            end
        )
        | "HOMEBOY_WORDPRESS_DEPENDENCY_" + join("_") + "_ROOT";
    def legacy_dependency_name:
        ascii_upcase | gsub("[^A-Z0-9_]"; "_") + "_PATH";
    .dependency_roots as $roots
    | ["WP_PATH", "HOMEBOY_WORDPRESS_DEPENDENCY_ROOTS_JSON", "PATH", "HOME", "PWD", "TMPDIR", "SHELL", "USER", "LOGNAME"] as $reserved
    | [$roots | keys[] | legacy_dependency_name]
    | group_by(.)
    | map(select(length == 1) | .[0]) as $unique_legacy_names
    | {
        WP_PATH: "/wordpress",
        HOMEBOY_WORDPRESS_DEPENDENCY_ROOTS_JSON: ($roots | tojson)
    }
    + reduce ($roots | to_entries[]) as $dependency (
        {};
        ($dependency.key | namespaced_dependency_name) as $namespaced_name
        | ($dependency.key | legacy_dependency_name) as $legacy_name
        | . + {($namespaced_name): $dependency.value}
        | if (($unique_legacy_names | index($legacy_name)) != null and ($reserved | index($legacy_name)) == null)
          then . + {($legacy_name): $dependency.value}
          else .
          end
    )' <<< "$RECIPE_INPUTS")"

echo "  Files: ${#smoke_files[@]}"
echo "  WordPress: ${WP_VERSION:-default}"
echo "  Per-file timeout: ${HOST_SMOKE_TIMEOUT_SECONDS}s"
echo ""

# Run every requested smoke even when one fails, so a single CI run surfaces
# the complete per-smoke pass/fail picture instead of stopping at the first
# failing smoke. Each smoke runs in its own wp-codebox recipe process via
# run_one_smoke, so a fatal bootstrap/redeclare error in one smoke cannot abort
# the others. Failures are collected and reported as an aggregated summary; the
# phase still exits non-zero if ANY smoke failed so CI goes red. (#4682)
passed=0
failed=0
failed_smokes=()
last_failure_exit=0
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
        failed=$((failed + 1))
        failed_smokes+=("${rel_path}:exit=${exit_code}")
        last_failure_exit="$exit_code"
    fi
done

echo ""
echo "HOST_SMOKE_SUMMARY:passed=${passed} failed=${failed}"
if [ "$failed" -ne 0 ]; then
    echo ""
    echo "Real-WordPress smoke tests failed (${failed} of $((passed + failed))):"
    for entry in "${failed_smokes[@]}"; do
        echo "  - ${entry%%:exit=*} (exit ${entry##*:exit=})"
    done
    exit "$last_failure_exit"
fi

echo "Real-WordPress smoke test run complete."
