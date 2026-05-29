#!/usr/bin/env bash
set -euo pipefail

# WP Codebox-backed WordPress bench runner.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${SCRIPT_DIR}/../lib/resolve-context.sh}"
FAILURE_TRAP_HELPER="${HOMEBOY_RUNTIME_FAILURE_TRAP:-}"
BENCH_HELPER_SH="${HOMEBOY_RUNTIME_BENCH_HELPER_SH:-${HOME}/.homeboy/runtime/bench-helper.sh}"
BENCH_RESULTS_ARTIFACTS_HELPER="${SCRIPT_DIR}/bench-results-artifacts.sh"
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

if printf '%s' "$settings_json" | jq -e '((.bench_site_mode // "fresh") == "installed")' >/dev/null 2>&1; then
    echo "Error: bench_site_mode=installed requires a persisted-site WP Codebox recipe contract." >&2
    FAILED_STEP="WP Codebox bench configuration"
    exit 1
fi

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

homeboy_wp_codebox_compile_scenario_manifests() {
    local entries_json
    entries_json=$(printf '%s' "$settings_json" | jq -c '
        .playground_scenario_manifests // .scenario_manifests // []
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
            echo "Error: playground_scenario_manifests[$index] must be a path string or object" >&2
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
PLAYGROUND_WORKLOADS_JSON=$(jq -nc --argjson declared "$PLAYGROUND_WORKLOADS_JSON" --argjson scenarios "$SCENARIO_MANIFEST_WORKLOADS_JSON" '$declared + $scenarios')

EXTRA_PLUGINS_JSON=$(jq -nc --arg source "$PLUGIN_PATH" --arg slug "$PLUGIN_SLUG" '[{source: $source, slug: $slug, activate: false}]')
MOUNTS_JSON="[]"
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
        MOUNTS_JSON=$(jq -nc --argjson mounts "$MOUNTS_JSON" --arg source "$mount_host" --arg target "$mount_to" '$mounts + [{source: $source, target: $target, mode: "readonly"}]')
    done < <(printf '%s' "$PLAYGROUND_FILE_MOUNTS_JSON" | jq -c '.[]')
fi

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
if [ ! -d "$BENCH_DIR" ] && ! printf '%s' "$PLAYGROUND_WORKLOADS_JSON" | jq -e 'type == "array" and length > 0' >/dev/null 2>&1; then
    echo "Warning: No bench workloads found for ${PLUGIN_PATH}" >&2
    if [ -n "${HOMEBOY_BENCH_RESULTS_FILE:-}" ]; then
        homeboy_write_empty_bench_results "$COMPONENT_ID" 0 "$RESULTS_FILE"
        homeboy_wordpress_emit_bench_results_artifacts "$RESULTS_FILE"
    fi
    exit 0
fi

echo "Running bench workloads via WP Codebox..."
echo "  Plugin: ${PLUGIN_SLUG} (${PLUGIN_PATH})"
echo "  Backend: wp-codebox (WordPress Playground runtime)"

DEPENDENCY_SLUGS_CSV=""
if [ ${#DEPENDENCY_SLUGS[@]} -gt 0 ]; then
    DEPENDENCY_SLUGS_CSV=$(IFS=,; printf '%s' "${DEPENDENCY_SLUGS[*]}")
fi

PLAYGROUND_BLUEPRINT_JSON="{}"
if [ "$settings_json" != "{}" ]; then
    PLAYGROUND_BLUEPRINT_JSON=$(printf '%s' "$settings_json" | jq -c '.playground_blueprint // {}' 2>/dev/null || echo "{}")
fi
RUNTIME_BLUEPRINT_JSON=$(jq -nc \
    --argjson base "$PLAYGROUND_BLUEPRINT_JSON" \
    --argjson scenario "$SCENARIO_MANIFEST_BLUEPRINT_JSON" \
    --argjson defines "$WP_CONFIG_DEFINES_JSON" '
    ($base + $scenario) as $merged |
    ($merged.steps // []) as $steps |
    if ($defines | length) > 0 then
        $merged + {steps: ($steps + [{step: "defineWpConfigConsts", consts: $defines}])}
    else
        $merged + {steps: $steps}
    end
')

WORKFLOW_STEP_JSON=$(jq -nc \
    --arg component "$COMPONENT_ID" \
    --arg slug "$PLUGIN_SLUG" \
    --arg iterations "$ITERATIONS" \
    --arg warmup "$WARMUP_ITERATIONS" \
    --arg dependencySlugs "$DEPENDENCY_SLUGS_CSV" \
    --argjson env "$BENCH_ENV_JSON" \
    --argjson workloads "$PLAYGROUND_WORKLOADS_JSON" '
    {
        command: "wordpress.bench",
        args: [
            "component-id=" + $component,
            "plugin-slug=" + $slug,
            "iterations=" + $iterations,
            "warmup=" + $warmup,
            "dependency-slugs=" + $dependencySlugs,
            "env-json=" + ($env | tostring),
            "workloads-json=" + ($workloads | tostring)
        ]
    }
')

RECIPE_FILE=$(mktemp "${TMPDIR:-/tmp}/homeboy-wp-codebox-bench-recipe.XXXXXX")
jq -n \
    --arg wp "$PLAYGROUND_WORDPRESS_VERSION" \
    --argjson blueprint "$RUNTIME_BLUEPRINT_JSON" \
    --argjson extraPlugins "$EXTRA_PLUGINS_JSON" \
    --argjson mounts "$MOUNTS_JSON" \
    --argjson step "$WORKFLOW_STEP_JSON" \
    '{
        schema: "wp-codebox/workspace-recipe/v1",
        runtime: {wp: $wp, blueprint: $blueprint},
        inputs: {extraPlugins: $extraPlugins, mounts: $mounts},
        workflow: {steps: [$step]}
    }' > "$RECIPE_FILE"

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

homeboy_wordpress_emit_bench_results_artifacts "$RESULTS_FILE"

rm -f "$WP_CODEBOX_TMPFILE"
rm -f "$RECIPE_FILE"
