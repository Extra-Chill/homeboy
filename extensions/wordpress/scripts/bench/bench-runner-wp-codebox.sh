#!/usr/bin/env bash
set -euo pipefail

# WP Codebox-backed WordPress bench runner.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${SCRIPT_DIR}/../lib/resolve-context.sh}"
FAILURE_TRAP_HELPER="${HOMEBOY_RUNTIME_FAILURE_TRAP:-}"
BENCH_HELPER_SH="${HOMEBOY_RUNTIME_BENCH_HELPER_SH:-${HOME}/.homeboy/runtime/bench-helper.sh}"
BENCH_RESULTS_ARTIFACTS_HELPER="${SCRIPT_DIR}/bench-results-artifacts.sh"
BENCH_BROWSER_METRICS_HELPER="${EXTENSION_DIR}/lib/wp-codebox-browser-metrics.js"
DEPENDENCY_HELPER="${HOMEBOY_WORDPRESS_DEPENDENCY_HELPER:-${SCRIPT_DIR}/../lib/validation-dependencies.sh}"
BENCH_BROWSER_TARGET_HELPER="${SCRIPT_DIR}/browser-target.sh"
BENCH_RECIPE_BUILDER="${SCRIPT_DIR}/build-wp-codebox-bench-recipe.mjs"

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
# shellcheck source=bench-results-artifacts.sh
source "$BENCH_RESULTS_ARTIFACTS_HELPER"
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

homeboy_wp_codebox_resolve_host_path() {
    local base_dir="$1"
    local path_value="$2"
    if [[ "$path_value" = /* ]]; then
        printf '%s\n' "$path_value"
    else
        printf '%s\n' "${base_dir}/${path_value}"
    fi
}

homeboy_wp_codebox_component_relative_path() {
    local host_path="$1"
    if [[ "$host_path" = "$PLUGIN_PATH"/* ]]; then
        printf '%s\n' "${host_path#"$PLUGIN_PATH/"}"
    else
        echo "Error: scenario manifest file references must stay under component root: $host_path" >&2
        FAILED_STEP="Scenario manifest setup"
        exit 1
    fi
}

homeboy_wp_codebox_compile_bootstrap_files() {
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
        WP_CODEBOX_BOOTSTRAP_FILES_JSON=$(jq -nc --arg file "$bootstrap_rel" '[$file]')
        return 0
    done < <(printf '%s' "$entries_json" | jq -r '.[] | select(type == "string" and . != "")')

    echo "Error: no wp_codebox_bootstrap_files entries exist under component root." >&2
    FAILED_STEP="WP Codebox bootstrap file setup"
    exit 1
}

homeboy_wp_codebox_compile_bootstrap_steps() {
    local steps_json
    steps_json=$(printf '%s' "$settings_json" | jq -c '
        .wp_codebox_bootstrap_steps // .wp_codebox_setup_steps // .bench_bootstrap_steps // []
        | if type == "array" then . else [] end
    ' 2>/dev/null || echo '[]')

    WP_CODEBOX_BOOTSTRAP_STEPS_JSON="[]"
    if ! printf '%s' "$steps_json" | jq -e 'type == "array" and length > 0' >/dev/null 2>&1; then
        return 0
    fi

    if ! printf '%s' "$steps_json" | jq -e '
        all(.[];
            type == "object"
            and (.command | type == "string" and . != "")
            and ((.args // []) | type == "array" and all(.[]; type == "string"))
        )
    ' >/dev/null 2>&1; then
        echo "Error: wp_codebox_bootstrap_steps entries require command plus optional string args array." >&2
        FAILED_STEP="WP Codebox bench bootstrap setup"
        exit 1
    fi

    WP_CODEBOX_BOOTSTRAP_STEPS_JSON="$steps_json"
}

homeboy_wp_codebox_compile_prepare_steps() {
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
        FAILED_STEP="WP Codebox bench prepare setup"
        exit 1
    fi

    WP_CODEBOX_PREPARE_STEPS_JSON="$steps_json"
}

homeboy_wp_codebox_prepare_step_cwd() {
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

homeboy_wp_codebox_emit_prepare_failure_diagnostic() {
    local step_index="$1"
    local command_name="$2"
    local cwd_host="$3"
    local output_file="$4"
    local exit_code="$5"
    local diagnostics_file="${ARTIFACTS_DIR}/wp-codebox-bench-prepare-diagnostics.json"
    local output_artifact="${ARTIFACTS_DIR}/wp-codebox-prepare-step-${step_index}-output.txt"

    mkdir -p "$ARTIFACTS_DIR"
    cp "$output_file" "$output_artifact"

    jq -n \
        --arg schema "homeboy/wordpress-bench-diagnostic/v1" \
        --arg code "wp-codebox-bench-prepare-failed" \
        --arg message "WP Codebox bench prepare step failed before the plugin was mounted into WordPress." \
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

    echo "WP Codebox bench prepare step failed before plugin runtime launch." >&2
    echo "  Step: ${step_index}" >&2
    echo "  Command: ${command_name}" >&2
    echo "  CWD: ${cwd_host}" >&2
    echo "  Exit code: ${exit_code}" >&2
    echo "  Diagnostics: ${diagnostics_file}" >&2
    echo "  Raw output: ${output_artifact}" >&2
}

homeboy_wp_codebox_run_prepare_steps() {
    if ! printf '%s' "$WP_CODEBOX_PREPARE_STEPS_JSON" | jq -e 'type == "array" and length > 0' >/dev/null 2>&1; then
        return 0
    fi

    local index=0
    local step_json command_name cwd_ref cwd_host output_file exit_code
    while IFS= read -r step_json; do
        command_name=$(printf '%s' "$step_json" | jq -r '.command')
        cwd_ref=$(printf '%s' "$step_json" | jq -r '.cwd // empty')
        if ! cwd_host=$(homeboy_wp_codebox_prepare_step_cwd "$cwd_ref"); then
            echo "Error: wp_codebox_prepare_steps[$index].cwd must be a directory under component root." >&2
            FAILED_STEP="WP Codebox bench prepare setup"
            exit 1
        fi

        output_file=$(mktemp "${TMPDIR:-/tmp}/homeboy-wp-codebox-prepare-${index}.XXXXXX")
        echo "Preparing WordPress bench plugin source: ${command_name}" >&2
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
            homeboy_wp_codebox_emit_prepare_failure_diagnostic "$index" "$command_name" "$cwd_host" "$output_file" "$exit_code"
            rm -f "$output_file"
            FAILED_STEP="WP Codebox bench prepare step"
            exit "$exit_code"
        fi

        rm -f "$output_file"
        index=$((index + 1))
    done < <(printf '%s' "$WP_CODEBOX_PREPARE_STEPS_JSON" | jq -c '.[]')
}

homeboy_wp_codebox_output_has_bootstrap_failure() {
    local output_file="$1"

    grep -Eq 'plugin-runtime\.setup|plugin runtime setup|Recipe plugin runtime setup\[[0-9]+\] failed|"phase"[[:space:]]*:[[:space:]]*"setup"' "$output_file"
}

homeboy_wp_codebox_emit_bootstrap_failure_diagnostic() {
    local output_file="$1"
    local exit_code="$2"
    local diagnostics_file="${ARTIFACTS_DIR}/wp-codebox-bench-bootstrap-diagnostics.json"
    local output_artifact="${ARTIFACTS_DIR}/wp-codebox-output.txt"
    local exit_artifact="${ARTIFACTS_DIR}/wp-codebox-exit-code.txt"

    mkdir -p "$ARTIFACTS_DIR"
    cp "$output_file" "$output_artifact"
    printf '%s\n' "$exit_code" > "$exit_artifact"

    jq -n \
        --arg schema "homeboy/wordpress-bench-diagnostic/v1" \
        --arg code "wp-codebox-bench-bootstrap-failed" \
        --arg message "WP Codebox bench bootstrap setup failed before measured workloads executed." \
        --argjson exitCode "$exit_code" \
        --arg artifactsDir "$ARTIFACTS_DIR" \
        --arg outputArtifact "$output_artifact" \
        --arg exitArtifact "$exit_artifact" \
        '{
            schema: $schema,
            diagnostics: [{
                code: $code,
                severity: "error",
                phase: "setup",
                message: $message,
                exit_code: $exitCode,
                artifacts: {
                    directory: $artifactsDir,
                    output: $outputArtifact,
                    exit_code: $exitArtifact
                }
            }]
        }' > "$diagnostics_file"

    echo "WP Codebox bench bootstrap setup failed before measured workloads executed." >&2
    echo "  Exit code: $exit_code" >&2
    echo "  Diagnostics: $diagnostics_file" >&2
    echo "  Raw output: $output_artifact" >&2
}

homeboy_wp_codebox_mount_extra_bench_workloads() {
    local workloads_value="${HOMEBOY_BENCH_EXTRA_WORKLOADS:-}"
    [ -n "$workloads_value" ] || return 0

    local workload_path
    IFS=':' read -r -a workload_paths <<< "$workloads_value"
    for workload_path in "${workload_paths[@]}"; do
        [ -n "$workload_path" ] || continue
        if [ ! -f "$workload_path" ]; then
            echo "Error: extra bench workload not found: $workload_path" >&2
            FAILED_STEP="WP Codebox bench workload setup"
            exit 1
        fi
        MOUNTS_JSON=$(jq -nc \
            --argjson mounts "$MOUNTS_JSON" \
            --arg source "$(dirname "$workload_path")" \
            --arg target "/wordpress/wp-content/plugins/${PLUGIN_SLUG}/tests/bench" \
            '$mounts + [{source: $source, target: $target, mode: "readonly"}]')
    done
}

homeboy_wp_codebox_extra_workload_scenarios_json() {
    local workloads_value="${HOMEBOY_BENCH_EXTRA_WORKLOADS:-}"
    local scenarios="[]"
    [ -n "$workloads_value" ] || {
        printf '%s\n' "$scenarios"
        return 0
    }

    local workload_path workload_name scenario_id
    IFS=':' read -r -a workload_paths <<< "$workloads_value"
    for workload_path in "${workload_paths[@]}"; do
        [ -n "$workload_path" ] || continue
        workload_name="$(basename "$workload_path")"
        scenario_id="${workload_name%.*}"
        scenarios=$(jq -nc \
            --argjson scenarios "$scenarios" \
            --arg id "$scenario_id" \
            --arg file "tests/bench/${workload_name}" \
            '$scenarios + [{id: $id, file: $file, source: "rig", iterations: 0, metrics: {}}]')
    done
    printf '%s\n' "$scenarios"
}

homeboy_wp_codebox_component_workload_scenarios_json() {
    local bench_dir="${PLUGIN_PATH}/tests/bench"
    local scenarios="[]"
    [ -d "$bench_dir" ] || {
        printf '%s\n' "$scenarios"
        return 0
    }

    local workload_path workload_name scenario_id
    for workload_path in "$bench_dir"/*.php; do
        [ -f "$workload_path" ] || continue
        workload_name="$(basename "$workload_path")"
        scenario_id="${workload_name%.*}"
        scenarios=$(jq -nc \
            --argjson scenarios "$scenarios" \
            --arg id "$scenario_id" \
            --arg file "tests/bench/${workload_name}" \
            '$scenarios + [{id: $id, file: $file, source: "component", iterations: 0, metrics: {}}]')
    done
    printf '%s\n' "$scenarios"
}

homeboy_wp_codebox_configured_workload_scenarios_json() {
    local workloads_json="$1"
    printf '%s' "$workloads_json" | jq -c '
        if type == "array" then . else [] end
        | map(select((.id? | type) == "string" and .id != ""))
        | map(
            {
                id: .id,
                source: (if ((.metadata? // {}) | has("scenario_manifest")) then "scenario-manifest" else "configured" end),
                iterations: 0,
                metrics: {}
            }
            + (if (.label? | type) == "string" and .label != "" then {label: .label} else {} end)
        )
    '
}

homeboy_wp_codebox_find_plugin_file() {
    local plugin_dir="$1"
    local candidate
    for candidate in "$plugin_dir"/*.php; do
        [ -f "$candidate" ] || continue
        if grep -q 'Plugin Name:' "$candidate"; then
            basename "$candidate"
            return 0
        fi
    done
    return 1
}

homeboy_wp_codebox_compile_scenario_manifests() {
    local entries_json
    entries_json=$(printf '%s' "$settings_json" | jq -c '
        .wp_codebox_scenario_manifests // .scenario_manifests // []
        | if type == "array" then . elif type == "string" or type == "object" then [.] else [] end
    ' 2>/dev/null || echo '[]')

    SCENARIO_MANIFEST_WORKLOADS_JSON="[]"
    SCENARIO_MANIFEST_BLUEPRINT_JSON="{}"
    if ! printf '%s' "$entries_json" | jq -e 'type == "array" and length > 0' >/dev/null 2>&1; then
        return 0
    fi

    local index=0
    while IFS= read -r manifest_entry; do
        [ -n "$manifest_entry" ] || continue
        local entry_type manifest_json manifest_host manifest_dir manifest_rel
        entry_type=$(printf '%s' "$manifest_entry" | jq -r 'type')
        manifest_dir="$PLUGIN_PATH"
        manifest_rel=""
        if [ "$entry_type" = "string" ]; then
            local manifest_ref
            manifest_ref=$(printf '%s' "$manifest_entry" | jq -r '.')
            manifest_host=$(homeboy_wp_codebox_resolve_host_path "$PLUGIN_PATH" "$manifest_ref")
            if [ ! -f "$manifest_host" ]; then
                echo "Error: scenario manifest not found: $manifest_host" >&2
                FAILED_STEP="Scenario manifest setup"
                exit 1
            fi
            manifest_dir=$(dirname "$manifest_host")
            manifest_rel=$(homeboy_wp_codebox_component_relative_path "$manifest_host")
            manifest_json=$(jq -c '.' "$manifest_host")
        elif [ "$entry_type" = "object" ]; then
            manifest_json=$(printf '%s' "$manifest_entry" | jq -c '.')
        else
            echo "Error: wp_codebox_scenario_manifests[$index] must be a path string or object" >&2
            FAILED_STEP="Scenario manifest setup"
            exit 1
        fi

        local prompt prompt_file_ref prompt_file_host prompt_file_rel
        prompt=$(printf '%s' "$manifest_json" | jq -r 'if (.prompt? | type) == "string" then .prompt else empty end')
        prompt_file_ref=$(printf '%s' "$manifest_json" | jq -r 'if ((.prompt_file? // .prompt_path?) | type) == "string" then (.prompt_file // .prompt_path) else empty end')
        prompt_file_rel=""
        if [ -z "$prompt" ] && [ -n "$prompt_file_ref" ]; then
            prompt_file_host=$(homeboy_wp_codebox_resolve_host_path "$manifest_dir" "$prompt_file_ref")
            if [ ! -f "$prompt_file_host" ]; then
                echo "Error: scenario prompt file not found: $prompt_file_host" >&2
                FAILED_STEP="Scenario manifest setup"
                exit 1
            fi
            prompt=$(<"$prompt_file_host")
            prompt_file_rel=$(homeboy_wp_codebox_component_relative_path "$prompt_file_host")
        fi

        local blueprint_json blueprint_ref blueprint_host blueprint_rel
        blueprint_json="{}"
        blueprint_rel=""
        if printf '%s' "$manifest_json" | jq -e '(.blueprint? | type) == "object"' >/dev/null 2>&1; then
            blueprint_json=$(printf '%s' "$manifest_json" | jq -c '.blueprint')
        else
            blueprint_ref=$(printf '%s' "$manifest_json" | jq -r 'if ((.blueprint? // .blueprint_file?) | type) == "string" then (.blueprint // .blueprint_file) else empty end')
            if [ -n "$blueprint_ref" ]; then
                blueprint_host=$(homeboy_wp_codebox_resolve_host_path "$manifest_dir" "$blueprint_ref")
                if [ ! -f "$blueprint_host" ]; then
                    echo "Error: scenario blueprint file not found: $blueprint_host" >&2
                    FAILED_STEP="Scenario manifest setup"
                    exit 1
                fi
                blueprint_json=$(jq -c '.' "$blueprint_host")
                blueprint_rel=$(homeboy_wp_codebox_component_relative_path "$blueprint_host")
            fi
        fi
        if printf '%s' "$blueprint_json" | jq -e 'length > 0' >/dev/null 2>&1; then
            SCENARIO_MANIFEST_BLUEPRINT_JSON=$(jq -nc --argjson base "$SCENARIO_MANIFEST_BLUEPRINT_JSON" --argjson next "$blueprint_json" '
                ($base + $next) | .steps = (($base.steps // []) + ($next.steps // []))
            ')
        fi

        local grader_ref grader_host grader_rel run_json verifier_refs_json verifier_rels_json
        grader_ref=$(printf '%s' "$manifest_json" | jq -r 'if ((.grader? // .grader_file?) | type) == "string" then (.grader // .grader_file) else empty end')
        grader_rel=""
        if [ -n "$grader_ref" ]; then
            grader_host=$(homeboy_wp_codebox_resolve_host_path "$manifest_dir" "$grader_ref")
            if [ ! -f "$grader_host" ]; then
                echo "Error: scenario grader file not found: $grader_host" >&2
                FAILED_STEP="Scenario manifest setup"
                exit 1
            fi
            grader_rel=$(homeboy_wp_codebox_component_relative_path "$grader_host")
        fi

        verifier_refs_json=$(printf '%s' "$manifest_json" | jq -c '
            [
                if ((.verifier? // .verifier_file?) | type) == "string" then (.verifier // .verifier_file) else empty end,
                if (.verifier_files? | type) == "array" then .verifier_files[] else empty end,
                if (.verifiers? | type) == "array" then .verifiers[] else empty end
            ] | map(select(type == "string" and . != ""))
        ')
        verifier_rels_json="[]"
        if printf '%s' "$verifier_refs_json" | jq -e 'length > 0' >/dev/null 2>&1; then
            local verifier_ref verifier_host verifier_rel
            while IFS= read -r verifier_ref; do
                [ -n "$verifier_ref" ] || continue
                verifier_host=$(homeboy_wp_codebox_resolve_host_path "$manifest_dir" "$verifier_ref")
                if [ ! -f "$verifier_host" ]; then
                    echo "Error: scenario verifier file not found: $verifier_host" >&2
                    FAILED_STEP="Scenario manifest setup"
                    exit 1
                fi
                verifier_rel=$(homeboy_wp_codebox_component_relative_path "$verifier_host")
                verifier_rels_json=$(jq -nc --argjson verifiers "$verifier_rels_json" --arg verifier "$verifier_rel" '$verifiers + [$verifier]')
            done < <(printf '%s' "$verifier_refs_json" | jq -r '.[]')
        fi

        run_json=$(printf '%s' "$manifest_json" | jq -c 'if (.run? | type) == "array" then .run else [] end')
        if [ -n "$grader_rel" ]; then
            run_json=$(jq -nc --argjson run "$run_json" --arg grader "$grader_rel" '$run + [{type: "php", file: $grader}]')
        fi
        if printf '%s' "$verifier_rels_json" | jq -e 'length > 0' >/dev/null 2>&1; then
            run_json=$(jq -nc --argjson run "$run_json" --argjson verifiers "$verifier_rels_json" '$run + ($verifiers | map({type: "php", file: .}))')
        fi
        if ! printf '%s' "$run_json" | jq -e 'type == "array" and length > 0' >/dev/null 2>&1; then
            echo "Error: scenario manifest requires run steps, a grader PHP file, or verifier PHP files" >&2
            FAILED_STEP="Scenario manifest setup"
            exit 1
        fi

        local workload_json
        workload_json=$(jq -nc \
            --argjson manifest "$manifest_json" \
            --argjson run "$run_json" \
            --arg prompt "$prompt" \
            --arg manifestFile "$manifest_rel" \
            --arg promptFile "$prompt_file_rel" \
            --arg blueprintFile "$blueprint_rel" \
            --arg graderFile "$grader_rel" \
            --argjson verifierFiles "$verifier_rels_json" \
            '($manifest.metadata // {}) as $metadata |
            {
                id: ($manifest.id // $manifest.label),
                label: $manifest.label,
                run: $run,
                tags: ($manifest.tags // []),
                artifacts: ($manifest.artifacts // {}),
                metadata: ($metadata + {
                    scenario_manifest: $manifestFile,
                    prompt: $prompt,
                    prompt_file: $promptFile,
                    blueprint_file: $blueprintFile,
                    grader_file: $graderFile,
                    verifier_files: $verifierFiles,
                    forbidden_mutations: ($manifest.forbidden_mutations // []),
                    required_active_plugins: ($manifest.required_active_plugins // []),
                    limits: ($manifest.limits // {}),
                    rules: ($manifest.rules // {}),
                    general_rules: ($manifest.general_rules // $manifest.rules.general // []),
                    task_rules: ($manifest.task_rules // $manifest.rules.task_specific // []),
                    probes: ($manifest.probes // {})
                })
            }
            | if .label == null then del(.label) else . end
            | .metadata |= with_entries(select(.value != "" and .value != {}))')
        SCENARIO_MANIFEST_WORKLOADS_JSON=$(jq -nc --argjson workloads "$SCENARIO_MANIFEST_WORKLOADS_JSON" --argjson workload "$workload_json" '$workloads + [$workload]')
        index=$((index + 1))
    done < <(printf '%s' "$entries_json" | jq -c '.[]')
}

homeboy_wp_codebox_compile_scenario_manifests
homeboy_wp_codebox_compile_bootstrap_files
homeboy_wp_codebox_compile_bootstrap_steps
homeboy_wp_codebox_compile_prepare_steps

WP_CODEBOX_WORDPRESS_VERSION="7.0"
if [ "$settings_json" != "{}" ]; then
    extracted=$(printf '%s' "$settings_json" | jq -r '.wp_codebox_wordpress_version // empty' 2>/dev/null || true)
    [ -n "$extracted" ] && [ "$extracted" != "null" ] && WP_CODEBOX_WORDPRESS_VERSION="$extracted"
fi

WP_CODEBOX_PHP_MEMORY_LIMIT="${HOMEBOY_WP_CODEBOX_PHP_MEMORY_LIMIT:-}"
if [ -z "$WP_CODEBOX_PHP_MEMORY_LIMIT" ] && [ "$settings_json" != "{}" ]; then
    extracted=$(printf '%s' "$settings_json" | jq -r '.wp_codebox_php_memory_limit // .wp_codebox_memory_limit // empty' 2>/dev/null || true)
    [ -n "$extracted" ] && [ "$extracted" != "null" ] && WP_CODEBOX_PHP_MEMORY_LIMIT="$extracted"
fi
if [ -n "$WP_CODEBOX_PHP_MEMORY_LIMIT" ] && ! printf '%s' "$WP_CODEBOX_PHP_MEMORY_LIMIT" | grep -Eq '^[0-9]+[KMG]?$'; then
    echo "Error: wp_codebox_php_memory_limit must be a PHP shorthand size such as 512M." >&2
    FAILED_STEP="WP Codebox PHP memory budget setup"
    exit 1
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

homeboy_wp_codebox_run_prepare_steps

if type homeboy_preflight_declared_validation_dependency_paths &>/dev/null; then
    if ! homeboy_preflight_declared_validation_dependency_paths "$ARTIFACTS_DIR" "bench"; then
        FAILED_STEP="WordPress dependency plugin preflight"
        exit 1
    fi
fi

wp_codebox_command=("$WP_CODEBOX_BIN")
case "$WP_CODEBOX_BIN" in
    *.js)
        wp_codebox_command=(node "$WP_CODEBOX_BIN")
        ;;
esac
WP_CODEBOX_RESOLVED_BIN="$WP_CODEBOX_BIN"
if resolved_wp_codebox_bin=$(command -v "$WP_CODEBOX_BIN" 2>/dev/null); then
    WP_CODEBOX_RESOLVED_BIN="$resolved_wp_codebox_bin"
fi

if type homeboy_export_validation_dependency_paths &>/dev/null; then
    homeboy_export_validation_dependency_paths "$PLUGIN_PATH"
fi
DEPENDENCY_PATHS="${HOMEBOY_WORDPRESS_DEPENDENCY_PATHS:-}"
if [ -n "$DEPENDENCY_PATHS" ] && type homeboy_prepare_validation_dependency_paths_for_wp_codebox_bench &>/dev/null; then
    DEPENDENCY_PATHS=$(homeboy_prepare_validation_dependency_paths_for_wp_codebox_bench "$DEPENDENCY_PATHS" "$ARTIFACTS_DIR")
fi
if [ -n "$DEPENDENCY_PATHS" ] && type homeboy_preflight_wordpress_dependency_plugins &>/dev/null; then
    if ! homeboy_preflight_wordpress_dependency_plugins "$DEPENDENCY_PATHS" "$ARTIFACTS_DIR" "bench"; then
        FAILED_STEP="WordPress dependency plugin preflight"
        exit 1
    fi
fi

WP_CONFIG_DEFINES_JSON="{}"
BENCH_ENV_JSON="{}"
WP_CODEBOX_WORKLOADS_JSON="[]"
if [ "$settings_json" != "{}" ]; then
    WP_CONFIG_DEFINES_JSON=$(printf '%s' "$settings_json" | jq -c '.wp_config_defines // {}' 2>/dev/null || echo "{}")
    BENCH_ENV_JSON=$(printf '%s' "$settings_json" | jq -c '.bench_env // {}' 2>/dev/null || echo "{}")
    WP_CODEBOX_WORKLOADS_JSON=$(printf '%s' "$settings_json" | jq -c '.wp_codebox_workloads // .playground_workloads // []' 2>/dev/null || echo "[]")
fi
WP_CODEBOX_WORKLOADS_JSON=$(jq -nc --argjson declared "$WP_CODEBOX_WORKLOADS_JSON" --argjson scenarios "$SCENARIO_MANIFEST_WORKLOADS_JSON" '$declared + $scenarios')

MOUNTS_JSON="[]"
COMPONENT_PLUGIN_FILE="$(homeboy_wp_codebox_find_plugin_file "$PLUGIN_PATH" || true)"
if [ -n "$COMPONENT_PLUGIN_FILE" ]; then
    EXTRA_PLUGINS_JSON=$(jq -nc --arg source "$PLUGIN_PATH" --arg slug "$PLUGIN_SLUG" --arg pluginFile "${PLUGIN_SLUG}/${COMPONENT_PLUGIN_FILE}" '[{source: $source, slug: $slug, pluginFile: $pluginFile, activate: false}]')
else
    EXTRA_PLUGINS_JSON="[]"
    MOUNTS_JSON=$(jq -nc --arg source "$PLUGIN_PATH" --arg target "/wordpress/wp-content/plugins/${PLUGIN_SLUG}" '[{source: $source, target: $target, mode: "readonly"}]')
fi
DEPENDENCY_SLUGS=()
if [ -n "$DEPENDENCY_PATHS" ]; then
    while IFS= read -r dep_path; do
        [ -n "$dep_path" ] || continue
        dep_slug="$(homeboy_get_validation_dependency_slug "$dep_path" || basename "$dep_path")"
        DEPENDENCY_SLUGS+=("$dep_slug")
        EXTRA_PLUGINS_JSON=$(jq -nc --argjson plugins "$EXTRA_PLUGINS_JSON" --arg source "$dep_path" --arg slug "$dep_slug" '$plugins + [{source: $source, slug: $slug, activate: false}]')
    done <<< "$DEPENDENCY_PATHS"
fi

PLUGIN_DB_PHP="${PLUGIN_PATH}/db.php"
if [ -f "$PLUGIN_DB_PHP" ]; then
    MOUNTS_JSON=$(jq -nc --argjson mounts "$MOUNTS_JSON" --arg source "$PLUGIN_DB_PHP" '$mounts + [{source: $source, target: "/wordpress/wp-content/db.php", mode: "readonly"}]')
fi

WP_CODEBOX_FILE_MOUNTS_JSON="[]"
if [ "$settings_json" != "{}" ]; then
    WP_CODEBOX_FILE_MOUNTS_JSON=$(printf '%s' "$settings_json" | jq -c '.wp_codebox_file_mounts // .playground_file_mounts // []' 2>/dev/null || echo "[]")
fi
if printf '%s' "$WP_CODEBOX_FILE_MOUNTS_JSON" | jq -e 'type == "array" and length > 0' >/dev/null 2>&1; then
    while IFS= read -r mount_json; do
        [ -n "$mount_json" ] || continue
        mount_from=$(printf '%s' "$mount_json" | jq -r '.from // empty')
        mount_to=$(printf '%s' "$mount_json" | jq -r '.to // empty')
        mount_dependency=$(printf '%s' "$mount_json" | jq -r '.from_dependency // empty')
        if [ -z "$mount_from" ] || [ -z "$mount_to" ] || [[ "$mount_from" = /* ]] || [[ "$mount_from" == *..* ]] || [[ "$mount_to" != /* ]]; then
            echo "Error: wp_codebox_file_mounts entries require relative 'from' and absolute 'to' paths." >&2
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
            echo "Error: wp_codebox_file_mounts dependency not found: $mount_dependency" >&2
            FAILED_STEP="WP Codebox file mount setup"
            exit 1
        fi
        mount_host="${mount_root}/${mount_from}"
        if [ ! -f "$mount_host" ]; then
            echo "Error: wp_codebox_file_mounts source file not found: $mount_host" >&2
            FAILED_STEP="WP Codebox file mount setup"
            exit 1
        fi
        MOUNTS_JSON=$(jq -nc --argjson mounts "$MOUNTS_JSON" --arg source "$mount_host" --arg target "$mount_to" '$mounts + [{source: $source, target: $target, mode: "readonly"}]')
    done < <(printf '%s' "$WP_CODEBOX_FILE_MOUNTS_JSON" | jq -c '.[]')
fi

homeboy_wp_codebox_mount_extra_bench_workloads

SHARED_STATE_HOST="${HOMEBOY_BENCH_SHARED_STATE:-}"
if [ -n "$SHARED_STATE_HOST" ]; then
    mkdir -p "$SHARED_STATE_HOST"
    MOUNTS_JSON=$(jq -nc --argjson mounts "$MOUNTS_JSON" --arg source "$SHARED_STATE_HOST" '$mounts + [{source: $source, target: "/bench-shared-state", mode: "readwrite"}]')
    BENCH_ENV_JSON=$(jq -nc --argjson env "$BENCH_ENV_JSON" --arg shared "/bench-shared-state" --arg instance "${HOMEBOY_BENCH_INSTANCE_ID:-0}" --arg concurrency "${HOMEBOY_BENCH_CONCURRENCY:-1}" '$env + {HOMEBOY_BENCH_SHARED_STATE: $shared, HOMEBOY_BENCH_INSTANCE_ID: $instance, HOMEBOY_BENCH_CONCURRENCY: $concurrency}')
    WP_CONFIG_DEFINES_JSON=$(jq -nc --argjson defines "$WP_CONFIG_DEFINES_JSON" --arg shared "/bench-shared-state" --arg instance "${HOMEBOY_BENCH_INSTANCE_ID:-0}" --arg concurrency "${HOMEBOY_BENCH_CONCURRENCY:-1}" '$defines + {HOMEBOY_BENCH_SHARED_STATE: $shared, HOMEBOY_BENCH_INSTANCE_ID: $instance, HOMEBOY_BENCH_CONCURRENCY: $concurrency}')
fi

if ! homeboy_wordpress_emit_browser_target "$settings_json" "$SHARED_STATE_HOST" "$COMPONENT_ID" "$PLUGIN_SLUG" "fresh"; then
    FAILED_STEP="Browser bench target setup"
    exit 1
fi

BENCH_DIR="${PLUGIN_PATH}/tests/bench"
HAS_EXTRA_BENCH_WORKLOADS=0
[ -n "${HOMEBOY_BENCH_EXTRA_WORKLOADS:-}" ] && HAS_EXTRA_BENCH_WORKLOADS=1

if [ "${HOMEBOY_BENCH_LIST_ONLY:-}" = "1" ]; then
    mkdir -p "$(dirname "$RESULTS_FILE")"
    jq -n \
        --arg component "$COMPONENT_ID" \
        --argjson componentScenarios "$(homeboy_wp_codebox_component_workload_scenarios_json)" \
        --argjson extraScenarios "$(homeboy_wp_codebox_extra_workload_scenarios_json)" \
        --argjson configuredScenarios "$(homeboy_wp_codebox_configured_workload_scenarios_json "$WP_CODEBOX_WORKLOADS_JSON")" \
        '$componentScenarios + $extraScenarios + $configuredScenarios as $scenarios |
        {component_id: $component, iterations: 0, scenarios: $scenarios}' > "$RESULTS_FILE"
    exit 0
fi

if [ ! -d "$BENCH_DIR" ] && [ "$HAS_EXTRA_BENCH_WORKLOADS" -eq 0 ] && ! printf '%s' "$WP_CODEBOX_WORKLOADS_JSON" | jq -e 'type == "array" and length > 0' >/dev/null 2>&1; then
    echo "Warning: No bench workloads found for ${PLUGIN_PATH}" >&2
    if [ -n "${HOMEBOY_BENCH_RESULTS_FILE:-}" ]; then
        homeboy_write_empty_bench_results "$COMPONENT_ID" 0 "$RESULTS_FILE"
        homeboy_wordpress_emit_bench_results_artifacts "$RESULTS_FILE"
    fi
    exit 0
fi

echo "Running bench workloads via WP Codebox..."
echo "  Plugin: ${PLUGIN_SLUG} (${PLUGIN_PATH})"
echo "  Backend: wp-codebox"

DEPENDENCY_SLUGS_CSV=""
if [ ${#DEPENDENCY_SLUGS[@]} -gt 0 ]; then
    DEPENDENCY_SLUGS_CSV=$(IFS=,; printf '%s' "${DEPENDENCY_SLUGS[*]}")
fi

WP_CODEBOX_BLUEPRINT_JSON="{}"
if [ "$settings_json" != "{}" ]; then
    WP_CODEBOX_BLUEPRINT_JSON=$(printf '%s' "$settings_json" | jq -c '.wp_codebox_blueprint // .playground_blueprint // {}' 2>/dev/null || echo "{}")
fi
RUNTIME_BLUEPRINT_JSON=$(jq -nc \
    --argjson base "$WP_CODEBOX_BLUEPRINT_JSON" \
    --argjson scenario "$SCENARIO_MANIFEST_BLUEPRINT_JSON" '
    ($base + $scenario) as $merged |
    $merged + {steps: ($merged.steps // [])}
')
WP_CODEBOX_PLUGIN_RUNTIME_JSON="{}"
if [ -n "$WP_CODEBOX_PHP_MEMORY_LIMIT" ]; then
    WP_CODEBOX_PLUGIN_RUNTIME_JSON=$(jq -nc --arg memoryLimit "$WP_CODEBOX_PHP_MEMORY_LIMIT" '{php: {memoryLimit: $memoryLimit}}')
fi
if printf '%s' "$WP_CODEBOX_BOOTSTRAP_STEPS_JSON" | jq -e 'type == "array" and length > 0' >/dev/null 2>&1; then
    WP_CODEBOX_PLUGIN_RUNTIME_JSON=$(jq -nc --argjson runtime "$WP_CODEBOX_PLUGIN_RUNTIME_JSON" --argjson setup "$WP_CODEBOX_BOOTSTRAP_STEPS_JSON" '$runtime + {setup: $setup}')
fi

homeboy_wp_codebox_emit_memory_fatal_diagnostic() {
    local output_file="$1"
    local exit_code="$2"
    local fatal_line="$3"
    local diagnostics_file="${ARTIFACTS_DIR}/wp-codebox-bench-diagnostics.json"
    local output_artifact="${ARTIFACTS_DIR}/wp-codebox-output.txt"
    local exit_artifact="${ARTIFACTS_DIR}/wp-codebox-exit-code.txt"
    local allowed_bytes=""
    local attempted_bytes=""
    local failing_file=""
    local failing_line=""

    mkdir -p "$ARTIFACTS_DIR"
    cp "$output_file" "$output_artifact"
    printf '%s\n' "$exit_code" > "$exit_artifact"

    allowed_bytes=$(printf '%s\n' "$fatal_line" | sed -E 's/.*Allowed memory size of ([0-9]+) bytes exhausted.*/\1/' || true)
    attempted_bytes=$(printf '%s\n' "$fatal_line" | sed -E 's/.*tried to allocate ([0-9]+) bytes.*/\1/' || true)
    failing_file=$(printf '%s\n' "$fatal_line" | sed -E 's/.* in ([^ ]+) on line [0-9]+.*/\1/' || true)
    failing_line=$(printf '%s\n' "$fatal_line" | sed -E 's/.* on line ([0-9]+).*/\1/' || true)
    [ "$allowed_bytes" != "$fatal_line" ] || allowed_bytes=""
    [ "$attempted_bytes" != "$fatal_line" ] || attempted_bytes=""
    [ "$failing_file" != "$fatal_line" ] || failing_file=""
    [ "$failing_line" != "$fatal_line" ] || failing_line=""

    jq -n \
        --arg schema "homeboy/wordpress-bench-diagnostic/v1" \
        --arg code "wp-codebox-php-memory-exhausted" \
        --arg message "WP Codebox wordpress.bench exhausted PHP memory before producing structured bench output." \
        --argjson exitCode "$exit_code" \
        --arg configuredMemoryLimit "$WP_CODEBOX_PHP_MEMORY_LIMIT" \
        --arg allowedBytes "$allowed_bytes" \
        --arg attemptedBytes "$attempted_bytes" \
        --arg failingFile "$failing_file" \
        --arg failingLine "$failing_line" \
        --arg artifactsDir "$ARTIFACTS_DIR" \
        --arg outputArtifact "$output_artifact" \
        --arg exitArtifact "$exit_artifact" \
        '{
            schema: $schema,
            diagnostics: [{
                code: $code,
                severity: "error",
                message: $message,
                exit_code: $exitCode,
                php_memory_limit: (if $configuredMemoryLimit == "" then null else $configuredMemoryLimit end),
                allowed_bytes: (if $allowedBytes == "" then null else ($allowedBytes | tonumber) end),
                attempted_allocation_bytes: (if $attemptedBytes == "" then null else ($attemptedBytes | tonumber) end),
                failing_file: (if $failingFile == "" then null else $failingFile end),
                failing_line: (if $failingLine == "" then null else ($failingLine | tonumber) end),
                artifacts: {
                    directory: $artifactsDir,
                    output: $outputArtifact,
                    exit_code: $exitArtifact
                }
            }]
        }' > "$diagnostics_file"

    echo "WP Codebox wordpress.bench exhausted PHP memory before producing structured output." >&2
    [ -z "$WP_CODEBOX_PHP_MEMORY_LIMIT" ] || echo "  Configured PHP memory limit: $WP_CODEBOX_PHP_MEMORY_LIMIT" >&2
    [ -z "$allowed_bytes" ] || echo "  Allowed bytes: $allowed_bytes" >&2
    [ -z "$failing_file" ] || echo "  Failing file: $failing_file" >&2
    [ -z "$failing_line" ] || echo "  Failing line: $failing_line" >&2
    echo "  Exit code: $exit_code" >&2
    echo "  Diagnostics: $diagnostics_file" >&2
    echo "  Raw output: $output_artifact" >&2
}

RECIPE_FILE=$(mktemp "${TMPDIR:-/tmp}/homeboy-wp-codebox-bench-recipe.XXXXXX")
jq -n \
    --arg wpCodeboxBin "$WP_CODEBOX_RESOLVED_BIN" \
    --arg wp "$WP_CODEBOX_WORDPRESS_VERSION" \
    --arg component "$COMPONENT_ID" \
    --arg slug "$PLUGIN_SLUG" \
    --argjson iterations "$ITERATIONS" \
    --argjson warmupIterations "$WARMUP_ITERATIONS" \
    --argjson blueprint "$RUNTIME_BLUEPRINT_JSON" \
    --argjson extraPlugins "$EXTRA_PLUGINS_JSON" \
    --argjson mounts "$MOUNTS_JSON" \
    --argjson env "$BENCH_ENV_JSON" \
    --argjson wpConfigDefines "$WP_CONFIG_DEFINES_JSON" \
    --argjson dependencySlugs "$(printf '%s\n' "$DEPENDENCY_SLUGS_CSV" | jq -R 'split(",") | map(select(. != ""))')" \
    --argjson bootstrapFiles "$WP_CODEBOX_BOOTSTRAP_FILES_JSON" \
    --argjson workloads "$WP_CODEBOX_WORKLOADS_JSON" \
    --argjson pluginRuntime "$WP_CODEBOX_PLUGIN_RUNTIME_JSON" \
    '{
        wpCodeboxBin: $wpCodeboxBin,
        options: {
            wordpressVersion: $wp,
            blueprint: $blueprint,
            mounts: $mounts,
            extraPlugins: $extraPlugins,
            componentId: $component,
            pluginSlug: $slug,
            iterations: $iterations,
            warmupIterations: $warmupIterations,
            dependencySlugs: $dependencySlugs,
            env: $env,
            wpConfigDefines: $wpConfigDefines,
            pluginRuntime: $pluginRuntime,
            bootstrapFiles: $bootstrapFiles,
            workloads: $workloads
        }
    }' | node "$BENCH_RECIPE_BUILDER" > "$RECIPE_FILE"

WP_CODEBOX_TMPFILE=$(mktemp)
set +e
"${wp_codebox_command[@]}" recipe-run \
    --recipe "$RECIPE_FILE" \
    --artifacts "$ARTIFACTS_DIR" \
    --json \
    > "$WP_CODEBOX_TMPFILE" 2>&1
wp_codebox_exit=$?
set -e

if [ $wp_codebox_exit -ne 0 ]; then
    cat "$WP_CODEBOX_TMPFILE" >&2
    if homeboy_wp_codebox_output_has_bootstrap_failure "$WP_CODEBOX_TMPFILE"; then
        homeboy_wp_codebox_emit_bootstrap_failure_diagnostic "$WP_CODEBOX_TMPFILE" "$wp_codebox_exit"
        FAILED_STEP="WP Codebox bench bootstrap setup"
        exit $wp_codebox_exit
    fi
    if fatal_line=$(grep -E '^(PHP Fatal error: |Fatal error: )?Allowed memory size of [0-9]+ bytes exhausted|^(PHP Fatal error: |Fatal error: ).*Allowed memory size of [0-9]+ bytes exhausted' "$WP_CODEBOX_TMPFILE" | head -1); then
        homeboy_wp_codebox_emit_memory_fatal_diagnostic "$WP_CODEBOX_TMPFILE" "$wp_codebox_exit" "$fatal_line"
    fi
    FAILED_STEP="WP Codebox bench run"
    exit $wp_codebox_exit
fi

if ! jq -e '.success == true and (.benchResults | type == "object")' "$WP_CODEBOX_TMPFILE" >/dev/null 2>&1; then
    cat "$WP_CODEBOX_TMPFILE" >&2
    FAILED_STEP="WP Codebox bench results parse"
    exit 1
fi

mkdir -p "$(dirname "$RESULTS_FILE")"
jq '.benchResults | del(.warmup_iterations)' "$WP_CODEBOX_TMPFILE" > "$RESULTS_FILE"

PREPARED_DEPENDENCIES_METADATA_FILE="${ARTIFACTS_DIR%/}/prepared-bench-dependencies.json"
if [ -f "$PREPARED_DEPENDENCIES_METADATA_FILE" ]; then
    PREPARED_RESULTS_FILE=$(mktemp "${TMPDIR:-/tmp}/homeboy-wp-codebox-prepared-dependencies.XXXXXX")
    if jq --slurpfile preparedDependencies "$PREPARED_DEPENDENCIES_METADATA_FILE" \
        '. + {prepared_dependencies: ($preparedDependencies[0] // [])}' \
        "$RESULTS_FILE" > "$PREPARED_RESULTS_FILE"; then
        mv "$PREPARED_RESULTS_FILE" "$RESULTS_FILE"
    else
        rm -f "$PREPARED_RESULTS_FILE"
        echo "Warning: failed to attach prepared dependency metadata to bench results." >&2
    fi
fi

if [ -f "$BENCH_BROWSER_METRICS_HELPER" ]; then
    ENRICHED_RESULTS_FILE=$(mktemp "${TMPDIR:-/tmp}/homeboy-wp-codebox-browser-metrics.XXXXXX")
    if node "$BENCH_BROWSER_METRICS_HELPER" "$RESULTS_FILE" "$ARTIFACTS_DIR" "$WP_CODEBOX_BIN" > "$ENRICHED_RESULTS_FILE"; then
        mv "$ENRICHED_RESULTS_FILE" "$RESULTS_FILE"
    else
        rm -f "$ENRICHED_RESULTS_FILE"
        echo "Warning: failed to parse WP Codebox browser artifacts; continuing with raw bench results." >&2
    fi
fi

homeboy_wordpress_emit_bench_results_artifacts "$RESULTS_FILE"

rm -f "$WP_CODEBOX_TMPFILE"
rm -f "$RECIPE_FILE"
