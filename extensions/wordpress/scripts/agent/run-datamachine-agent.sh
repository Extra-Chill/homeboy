#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
WORKLOAD_PATH="$SCRIPT_DIR/datamachine-agent-workload.php"
REPLAY_BUNDLE_BUILDER="$SCRIPT_DIR/build-replay-bundle.js"

homeboy_datamachine_agent_bundle_clone_url() {
    local repo_url="${1:-}"

    if [ -z "${GITHUB_TOKEN:-}" ]; then
        printf '%s\n' "$repo_url"
        return 0
    fi

    case "$repo_url" in
        https://github.com/*)
            printf 'https://x-access-token:%s@github.com/%s\n' "$GITHUB_TOKEN" "${repo_url#https://github.com/}"
            ;;
        http://github.com/*)
            printf 'https://x-access-token:%s@github.com/%s\n' "$GITHUB_TOKEN" "${repo_url#http://github.com/}"
            ;;
        git@github.com:*)
            printf 'https://x-access-token:%s@github.com/%s\n' "$GITHUB_TOKEN" "${repo_url#git@github.com:}"
            ;;
        *)
            printf '%s\n' "$repo_url"
            ;;
    esac
}

homeboy_datamachine_agent_wp_codebox_secret_env_names() {
    jq -r '
        [
            "HOMEBOY_DATAMACHINE_AGENT_CONFIG",
            .prompt_env?,
            (.github_token_env? // "GITHUB_TOKEN"),
            .github_repository_token_env?,
            (.provider_credentials? // {} | to_entries[]? | .value)
        ]
        | map(select(type == "string" and . != ""))
        | unique[]
    ' <<<"$CONFIG_JSON"
}

homeboy_datamachine_agent_wp_codebox_run() {
    local wp_codebox_bin="${HOMEBOY_WP_CODEBOX_BIN:-}"
    if [ -z "$wp_codebox_bin" ]; then
        wp_codebox_bin=$(jq -r '.wp_codebox_bin // "wp-codebox"' "$CONFIG_PATH")
    fi
    if [ "$wp_codebox_bin" = "wp-codebox" ] && ! command -v wp-codebox >/dev/null 2>&1; then
        echo "ERROR: wp-codebox not found; set HOMEBOY_WP_CODEBOX_BIN or config wp_codebox_bin" >&2
        exit 1
    fi

    local artifacts_dir="${HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR:-}"
    if [ -z "$artifacts_dir" ]; then
        artifacts_dir=$(jq -r '.wp_codebox_artifacts_dir // empty' "$CONFIG_PATH")
    fi
    if [ -z "$artifacts_dir" ]; then
        if [ -z "$RUNTIME_DIR" ]; then
            RUNTIME_DIR=$(mktemp -d "${TMPDIR:-/tmp}/homeboy-datamachine-agent-runtime.XXXXXX")
        fi
        artifacts_dir="$RUNTIME_DIR/wp-codebox-artifacts"
    fi
    if [ -z "$RUNTIME_DIR" ]; then
        RUNTIME_DIR=$(mktemp -d "${TMPDIR:-/tmp}/homeboy-datamachine-agent-runtime.XXXXXX")
    fi

    local workload_wrapper="$RUNTIME_DIR/wp-codebox-datamachine-agent-workload.php"
    cat > "$workload_wrapper" <<'PHP'
<?php
$homeboy_workload_result = require '/homeboy-extension/scripts/agent/datamachine-agent-workload.php';
echo wp_json_encode( is_array( $homeboy_workload_result ) ? $homeboy_workload_result : array() );
PHP

    local agents_api_path="${HOMEBOY_AGENTS_API_PATH:-${AGENTS_API_PATH:-}}"
    local wp_ai_client_path="${HOMEBOY_WP_AI_CLIENT_PATH:-${WP_AI_CLIENT_PATH:-}}"
    local data_machine_path="${HOMEBOY_DATA_MACHINE_PATH:-${DATA_MACHINE_PATH:-}}"
    local data_machine_code_path="${HOMEBOY_DATA_MACHINE_CODE_PATH:-${DATA_MACHINE_CODE_PATH:-}}"
    if [ -z "$agents_api_path" ]; then
        agents_api_path=$(jq -r '.wp_codebox_components.agents_api // .wp_codebox_agents_api_path // empty' "$CONFIG_PATH")
    fi
    if [ -z "$data_machine_path" ]; then
        data_machine_path=$(jq -r '.wp_codebox_components.data_machine // .wp_codebox_data_machine_path // empty' "$CONFIG_PATH")
    fi
    if [ -z "$wp_ai_client_path" ]; then
        wp_ai_client_path=$(jq -r '.wp_codebox_components.wp_ai_client // .wp_codebox_wp_ai_client_path // empty' "$CONFIG_PATH")
    fi
    if [ -z "$data_machine_code_path" ]; then
        data_machine_code_path=$(jq -r '.wp_codebox_components.data_machine_code // .wp_codebox_data_machine_code_path // empty' "$CONFIG_PATH")
    fi
    if [ -z "$agents_api_path" ] || [ -z "$data_machine_path" ] || [ -z "$data_machine_code_path" ]; then
        echo "ERROR: wp-codebox runtime requires Agents API, Data Machine, and Data Machine Code paths via env or config" >&2
        exit 1
    fi

    local recipe_file
    recipe_file=$(mktemp "${TMPDIR:-/tmp}/homeboy-wp-codebox-agent-recipe.XXXXXX")

    local extra_plugins_json mounts_json secret_env_json provider_slugs_csv
    extra_plugins_json=$(jq -nc \
        --arg agents "$agents_api_path" \
        --arg wpAiClient "$wp_ai_client_path" \
        --arg datamachine "$data_machine_path" \
        --arg code "$data_machine_code_path" \
        '[
            {source: $agents, slug: "agents-api", activate: false},
            (if $wpAiClient != "" then {source: $wpAiClient, slug: "php-ai-client", pluginFile: "php-ai-client/plugin.php"} else empty end),
            {source: $datamachine, slug: "data-machine", activate: false},
            {source: $code, slug: "data-machine-code", activate: false}
        ]')
    mounts_json=$(jq -nc --arg source "$EXTENSION_PATH" '[{source: $source, target: "/homeboy-extension", mode: "readonly"}]')
    provider_slugs_csv=""

    if [ -n "$BUNDLE_PATH" ]; then
        mounts_json=$(jq -nc --argjson mounts "$mounts_json" --arg source "$BUNDLE_PATH" --arg target "$BUNDLE_GUEST_PATH" '$mounts + [{source: $source, target: $target, mode: "readonly"}]')
    fi

    local extra_mount
    while IFS= read -r extra_mount; do
        IFS=: read -r mount_source mount_target mount_mode <<<"$extra_mount"
        [ -n "$mount_source" ] && [ -n "$mount_target" ] || continue
        mount_mode="${mount_mode:-readwrite}"
        mounts_json=$(jq -nc --argjson mounts "$mounts_json" --arg source "$mount_source" --arg target "$mount_target" --arg mode "$mount_mode" '$mounts + [{source: $source, target: $target, mode: $mode}]')
    done < <(jq -r '.wp_codebox_mounts? // [] | .[]? | select(type == "string" and . != "")' <<<"$CONFIG_JSON")

    local provider_plugin_path provider_slug
    while IFS= read -r provider_plugin_path; do
        provider_slug=$(basename "$provider_plugin_path")
        if [ -n "$provider_slugs_csv" ]; then
            provider_slugs_csv="${provider_slugs_csv},${provider_slug}"
        else
            provider_slugs_csv="$provider_slug"
        fi
        extra_plugins_json=$(jq -nc --argjson plugins "$extra_plugins_json" --arg source "$provider_plugin_path" --arg slug "$provider_slug" '$plugins + [{source: $source, slug: $slug, pluginFile: ($slug + "/plugin.php")} ]')
    done < <(jq -r '.provider_plugin_paths? // [] | .[]? | select(type == "string" and . != "")' <<<"$CONFIG_JSON")

    secret_env_json="[]"
    local secret_env_name
    while IFS= read -r secret_env_name; do
        secret_env_json=$(jq -nc --argjson names "$secret_env_json" --arg name "$secret_env_name" '$names + [$name]')
    done < <(homeboy_datamachine_agent_wp_codebox_secret_env_names)

    jq -n \
        --arg wp "$PLAYGROUND_WORDPRESS_VERSION" \
        --argjson extraPlugins "$extra_plugins_json" \
        --argjson mounts "$mounts_json" \
        --argjson secretEnv "$secret_env_json" \
        --arg task "$WORKLOAD_LABEL" \
        --arg codeFile "$workload_wrapper" \
        --arg providerSlugs "$provider_slugs_csv" \
        '{
            schema: "wp-codebox/workspace-recipe/v1",
            runtime: {wp: $wp, blueprint: {steps: []}},
            inputs: {extraPlugins: $extraPlugins, mounts: $mounts, secretEnv: $secretEnv},
            workflow: {steps: [{command: "wp-codebox.agent-sandbox-run", args: ["task=" + $task, "code-file=" + $codeFile, "provider-plugin-slugs=" + $providerSlugs]}]}
        }' >"$recipe_file"

    local wp_codebox_args=(
        recipe-run
        --recipe "$recipe_file"
        --artifacts "$artifacts_dir"
        --json
    )

    local wp_codebox_output
    wp_codebox_output=$(mktemp "${TMPDIR:-/tmp}/homeboy-wp-codebox-output.XXXXXX")
    local wp_codebox_command=("$wp_codebox_bin")
    case "$wp_codebox_bin" in
        *.js)
            wp_codebox_command=(node "$wp_codebox_bin")
            ;;
    esac

    set +e
    HOMEBOY_DATAMACHINE_AGENT_CONFIG="$CONFIG_JSON" \
        "${wp_codebox_command[@]}" "${wp_codebox_args[@]}" \
        >"$wp_codebox_output"
    local wp_codebox_exit=$?
    set -e
    rm -f "$recipe_file"

    if [ $wp_codebox_exit -ne 0 ]; then
        cat "$wp_codebox_output" >&2
        exit $wp_codebox_exit
    fi

    local wp_codebox_review_input="$RUNTIME_DIR/wp-codebox-review.json"
    local wp_codebox_changed_files_input="$RUNTIME_DIR/wp-codebox-changed-files.json"
    local wp_codebox_artifacts_json="$RUNTIME_DIR/wp-codebox-artifacts.json"
    printf 'null\n' >"$wp_codebox_review_input"
    printf 'null\n' >"$wp_codebox_changed_files_input"

    local wp_codebox_review_path
    local wp_codebox_changed_files_path
    wp_codebox_review_path=$(jq -r '.artifacts.reviewPath // empty' "$wp_codebox_output")
    wp_codebox_changed_files_path=$(jq -r '.artifacts.changedFilesPath // empty' "$wp_codebox_output")
    if [ -n "$wp_codebox_review_path" ] && [ -f "$wp_codebox_review_path" ]; then
        cp "$wp_codebox_review_path" "$wp_codebox_review_input"
    fi
    if [ -n "$wp_codebox_changed_files_path" ] && [ -f "$wp_codebox_changed_files_path" ]; then
        cp "$wp_codebox_changed_files_path" "$wp_codebox_changed_files_input"
    fi

    jq -n \
        --slurpfile run "$wp_codebox_output" \
        --slurpfile review "$wp_codebox_review_input" \
        --slurpfile changedFiles "$wp_codebox_changed_files_input" \
        '
        ($run[0].artifacts // {}) as $artifacts
        | {
            schema: "homeboy/wp-codebox-artifacts/v1",
            success: ($run[0].success // false),
            runtime: ($run[0].runtime // null),
            paths: {
                directory: ($artifacts.directory // ""),
                manifest: ($artifacts.manifestPath // ""),
                metadata: ($artifacts.metadataPath // ""),
                blueprint_after: ($artifacts.blueprintAfterPath // ""),
                blueprint_after_notes: ($artifacts.blueprintAfterNotesPath // ""),
                events: ($artifacts.eventsPath // ""),
                commands: ($artifacts.commandsPath // ""),
                observations: ($artifacts.observationsPath // ""),
                runtime_log: ($artifacts.runtimeLogPath // ""),
                commands_log: ($artifacts.commandsLogPath // ""),
                mounts: ($artifacts.mountsPath // ""),
                captured_mounts: ($artifacts.capturedMountsPath // ""),
                diffs: ($artifacts.diffsPath // ""),
                changed_files: ($artifacts.changedFilesPath // ""),
                patch: ($artifacts.patchPath // ""),
                review: ($artifacts.reviewPath // "")
            } | with_entries(select(.value != "")),
            review_payload: ($review[0] // null),
            changed_files: ($changedFiles[0] // null)
        }
        ' >"$wp_codebox_artifacts_json"

    jq -n \
        --arg component "$COMPONENT_ID" \
        --arg workloadId "$WORKLOAD_ID" \
        --arg workloadLabel "$WORKLOAD_LABEL" \
        --slurpfile run "$wp_codebox_output" \
        --slurpfile wpCodeboxArtifacts "$wp_codebox_artifacts_json" \
        '
        def parsed_workload:
            (($run[0].executions // [])[-1].stdout? // "{}" | fromjson? // {}) as $outer
            | ($outer.output? // "{}" | fromjson? // {});
        def artifact_entry($path; $kind; $label):
            { path: $path, kind: $kind, label: $label };
        parsed_workload as $workload
        | ($wpCodeboxArtifacts[0] // {}) as $wpArtifacts
        | ($wpArtifacts.paths // {}) as $wpPaths
        | {
            component_id: $component,
            iterations: 1,
            scenarios: [
                {
                    id: $workloadId,
                    label: $workloadLabel,
                    metrics: (($workload.metrics // {}) | with_entries(.key |= . + "_mean")),
                    artifacts: ({
                        wp_codebox_manifest: artifact_entry(($wpPaths.manifest // ""); "manifest"; "WP Codebox manifest"),
                        wp_codebox_metadata: artifact_entry(($wpPaths.metadata // ""); "metadata"; "WP Codebox metadata"),
                        wp_codebox_review: artifact_entry(($wpPaths.review // ""); "review"; "WP Codebox review payload"),
                        wp_codebox_changed_files: artifact_entry(($wpPaths.changed_files // ""); "changed-files"; "WP Codebox changed files"),
                        wp_codebox_patch: artifact_entry(($wpPaths.patch // ""); "patch"; "WP Codebox patch")
                    } | with_entries(select(.value.path != ""))),
                    metadata: (($workload.metadata // {}) + {
                        wp_codebox: {
                            success: ($run[0].success // false),
                            runtime: ($run[0].runtime // null),
                            artifacts: ($run[0].artifacts // null),
                            canonical_artifacts: ($wpPaths // {}),
                            review_payload: ($wpArtifacts.review_payload // null),
                            changed_files: ($wpArtifacts.changed_files // null),
                            error: ($run[0].error // null)
                        }
                    })
                }
            ]
        }
        ' >"$RESULTS_FILE"
}

CONFIG_PATH="${HOMEBOY_DATAMACHINE_AGENT_CONFIG_PATH:-}"
if [ -z "$CONFIG_PATH" ] && [ "${1:-}" != "" ]; then
    CONFIG_PATH="$1"
fi
if [ -z "$CONFIG_PATH" ] || [ ! -s "$CONFIG_PATH" ]; then
    echo "ERROR: pass a Data Machine agent config JSON path as argv[1] or HOMEBOY_DATAMACHINE_AGENT_CONFIG_PATH" >&2
    exit 1
fi
if ! command -v jq >/dev/null 2>&1; then
    echo "ERROR: jq required" >&2
    exit 1
fi
if [ ! -f "$WORKLOAD_PATH" ]; then
    echo "ERROR: Data Machine agent workload missing at $WORKLOAD_PATH" >&2
    exit 1
fi

RUNTIME_DIR=""
TERMINAL_SERVER_PID=""
cleanup() {
    if [ -n "$TERMINAL_SERVER_PID" ]; then
        kill "$TERMINAL_SERVER_PID" >/dev/null 2>&1 || true
    fi
    if [ -n "$RUNTIME_DIR" ]; then
        rm -rf "$RUNTIME_DIR"
    fi
}
trap cleanup EXIT

if [ -z "${HOMEBOY_RUNTIME_BENCH_HELPER_SH:-}" ] || [ -z "${HOMEBOY_RUNTIME_BENCH_HELPER_PHP:-}" ]; then
    RUNTIME_DIR=$(mktemp -d "${TMPDIR:-/tmp}/homeboy-datamachine-agent-runtime.XXXXXX")
    if [ -z "${HOMEBOY_RUNTIME_BENCH_HELPER_SH:-}" ]; then
        cat > "$RUNTIME_DIR/bench-helper.sh" <<'SH'
#!/usr/bin/env bash
homeboy_write_empty_bench_results() {
    printf '{"component":"%s","iterations":%s,"scenarios":[]}\n' "$1" "$2" > "$3"
}
SH
        export HOMEBOY_RUNTIME_BENCH_HELPER_SH="$RUNTIME_DIR/bench-helper.sh"
    fi
    if [ -z "${HOMEBOY_RUNTIME_BENCH_HELPER_PHP:-}" ]; then
        cat > "$RUNTIME_DIR/bench-helper.php" <<'PHP'
<?php
function homeboy_bench_percentile(array $sorted_values, float $p): float {
    $n = count($sorted_values);
    if ($n === 0) {
        return 0.0;
    }
    if ($n === 1) {
        return (float) $sorted_values[0];
    }
    $rank = $p * ($n - 1);
    $lo = (int) floor($rank);
    $hi = (int) ceil($rank);
    if ($lo === $hi) {
        return (float) $sorted_values[$lo];
    }
    $frac = $rank - $lo;
    return (float) ($sorted_values[$lo] * (1 - $frac) + $sorted_values[$hi] * $frac);
}
function homeboy_bench_scenario_id(string $basename): string {
    $name = preg_replace('/\.[^.]+$/', '', $basename);
    $name = preg_replace('/([a-z0-9])([A-Z])/', '$1-$2', $name);
    $name = strtolower($name);
    $name = preg_replace('/[^a-z0-9]+/', '-', $name);
    return trim($name, '-');
}
function homeboy_write_bench_results(string $results_path, string $component_id, int $iterations, array $scenarios): void {
    file_put_contents($results_path, json_encode([
        'component_id' => $component_id,
        'iterations' => $iterations,
        'scenarios' => $scenarios,
    ], JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES));
}
PHP
        export HOMEBOY_RUNTIME_BENCH_HELPER_PHP="$RUNTIME_DIR/bench-helper.php"
    fi
fi

RESULTS_FILE="${HOMEBOY_DATAMACHINE_AGENT_RESULTS_FILE:-${HOMEBOY_BENCH_RESULTS_FILE:-}}"
if [ -z "$RESULTS_FILE" ]; then
    RESULTS_FILE=$(mktemp "${TMPDIR:-/tmp}/homeboy-datamachine-agent.XXXXXX")
    PRINT_RESULTS=1
else
    PRINT_RESULTS=0
fi

COMPONENT_PATH=$(jq -r '.component_path // env.HOMEBOY_COMPONENT_PATH // empty' "$CONFIG_PATH")
COMPONENT_ID=$(jq -r '.component_id // env.HOMEBOY_COMPONENT_ID // empty' "$CONFIG_PATH")
if [ -z "$COMPONENT_PATH" ]; then
    COMPONENT_PATH="$(pwd)"
fi
if [ -z "$COMPONENT_ID" ]; then
    COMPONENT_ID="$(basename "$COMPONENT_PATH")"
fi

CONFIG_JSON=$(jq -c . "$CONFIG_PATH")
BUNDLE_PATH=""
BUNDLE_GUEST_PATH=""
BUNDLE_REPO=$(jq -r '.bundle_repo // empty' "$CONFIG_PATH")
if [ -n "$BUNDLE_REPO" ]; then
    BUNDLE_REF=$(jq -r '.bundle_ref // "main"' "$CONFIG_PATH")
    BUNDLE_PATH_IN_REPO=$(jq -r '.bundle_path_in_repo // "."' "$CONFIG_PATH")
    if ! command -v git >/dev/null 2>&1; then
        echo "ERROR: git required for bundle_repo" >&2
        exit 1
    fi
    if [ -z "$RUNTIME_DIR" ]; then
        RUNTIME_DIR=$(mktemp -d "${TMPDIR:-/tmp}/homeboy-datamachine-agent-runtime.XXXXXX")
    fi
    BUNDLE_CHECKOUT_DIR="$RUNTIME_DIR/bundle-repo"
    BUNDLE_CLONE_URL=$(homeboy_datamachine_agent_bundle_clone_url "$BUNDLE_REPO")
    git clone --quiet "$BUNDLE_CLONE_URL" "$BUNDLE_CHECKOUT_DIR"
    git -C "$BUNDLE_CHECKOUT_DIR" checkout --quiet "$BUNDLE_REF"
    BUNDLE_PATH="$BUNDLE_CHECKOUT_DIR/$BUNDLE_PATH_IN_REPO"
    if [ ! -d "$BUNDLE_PATH" ]; then
        echo "ERROR: bundle_path_in_repo does not exist in bundle_repo: $BUNDLE_PATH_IN_REPO" >&2
        exit 1
    fi
    BUNDLE_GUEST_SLUG="$(basename "$(cd "$BUNDLE_PATH" && pwd)")"
    BUNDLE_GUEST_PATH="/wordpress/wp-content/plugins/${BUNDLE_GUEST_SLUG}"
    CONFIG_JSON=$(jq -c \
        --arg bundlePath "$BUNDLE_PATH" \
        --arg bundleGuestPath "$BUNDLE_GUEST_PATH" \
        --arg bundleRepo "$BUNDLE_REPO" \
        --arg bundleRef "$BUNDLE_REF" \
        --arg bundlePathInRepo "$BUNDLE_PATH_IN_REPO" \
        '. + {
            bundle_path: $bundleGuestPath,
            bundle_host_path: $bundlePath,
            bundle_repo: $bundleRepo,
            bundle_ref: $bundleRef,
            bundle_path_in_repo: $bundlePathInRepo
        } | .validation_dependencies = (
            (if (.validation_dependencies // .dependencies // []) | type == "array" then (.validation_dependencies // .dependencies // []) elif (.validation_dependencies // .dependencies // null) | type == "string" then [(.validation_dependencies // .dependencies)] else [] end)
            + [$bundlePath]
        )' <<<"$CONFIG_JSON")
else
    BUNDLE_HOST_PATH=$(jq -r '.bundle_host_path // empty' "$CONFIG_PATH")
    BUNDLE_PATH_IS_HOST_PATH=0
    if [ -z "$BUNDLE_HOST_PATH" ]; then
        BUNDLE_CONFIG_PATH=$(jq -r '.bundle_path // empty' "$CONFIG_PATH")
        if [ -n "$BUNDLE_CONFIG_PATH" ] && [ -d "$BUNDLE_CONFIG_PATH" ]; then
            BUNDLE_HOST_PATH="$BUNDLE_CONFIG_PATH"
            BUNDLE_PATH_IS_HOST_PATH=1
        fi
    fi
    if [ -n "$BUNDLE_HOST_PATH" ]; then
        BUNDLE_PATH="$BUNDLE_HOST_PATH"
        BUNDLE_GUEST_SLUG="$(basename "$(cd "$BUNDLE_PATH" && pwd)")"
        BUNDLE_GUEST_PATH=$(jq -r --arg fallback "/wordpress/wp-content/plugins/${BUNDLE_GUEST_SLUG}" --argjson bundlePathIsHostPath "$BUNDLE_PATH_IS_HOST_PATH" 'if $bundlePathIsHostPath then $fallback else (.bundle_path // $fallback) end' "$CONFIG_PATH")
        CONFIG_JSON=$(jq -c \
            --arg bundlePath "$BUNDLE_PATH" \
            --arg bundleGuestPath "$BUNDLE_GUEST_PATH" \
            '. + {
                bundle_path: $bundleGuestPath,
                bundle_host_path: $bundlePath
            } | .validation_dependencies = (
                (if (.validation_dependencies // .dependencies // []) | type == "array" then (.validation_dependencies // .dependencies // []) elif (.validation_dependencies // .dependencies // null) | type == "string" then [(.validation_dependencies // .dependencies)] else [] end)
                + [$bundlePath]
            )' <<<"$CONFIG_JSON")
    fi
fi

WORKLOAD_ID=$(jq -r '.workload_id // "datamachine-agent"' "$CONFIG_PATH")
WORKLOAD_LABEL=$(jq -r '.workload_label // "Run Data Machine agent"' "$CONFIG_PATH")
PLAYGROUND_WORDPRESS_VERSION=$(jq -r '.playground_wordpress_version // "7.0"' "$CONFIG_PATH")
ENABLE_TERMINAL_ACTIONS=$(jq -r 'if (.enable_terminal_actions // .enable_wp_cli_tool // false) then "1" else "0" end' <<<"$CONFIG_JSON")
if [ "$ENABLE_TERMINAL_ACTIONS" = "1" ]; then
    if [ -z "$RUNTIME_DIR" ]; then
        RUNTIME_DIR=$(mktemp -d "${TMPDIR:-/tmp}/homeboy-datamachine-agent-runtime.XXXXXX")
    fi
    TERMINAL_READY_FILE="$RUNTIME_DIR/terminal-action-server.json"
    TERMINAL_TOKEN="$(LC_ALL=C tr -dc 'A-Za-z0-9' </dev/urandom | head -c 32)"
    node "$SCRIPT_DIR/terminal-action-server.js" \
        --runtime-root "$COMPONENT_PATH" \
        --ready-file "$TERMINAL_READY_FILE" \
        --token "$TERMINAL_TOKEN" \
        >"$RUNTIME_DIR/terminal-action-server.log" 2>&1 &
    TERMINAL_SERVER_PID=$!
    for _ in $(seq 1 100); do
        if [ -s "$TERMINAL_READY_FILE" ]; then
            break
        fi
        if ! kill -0 "$TERMINAL_SERVER_PID" >/dev/null 2>&1; then
            echo "ERROR: terminal action server exited before becoming ready" >&2
            cat "$RUNTIME_DIR/terminal-action-server.log" >&2 || true
            exit 1
        fi
        sleep 0.1
    done
    if [ ! -s "$TERMINAL_READY_FILE" ]; then
        echo "ERROR: terminal action server did not become ready" >&2
        cat "$RUNTIME_DIR/terminal-action-server.log" >&2 || true
        exit 1
    fi
    TERMINAL_URL=$(jq -r '.url' "$TERMINAL_READY_FILE")
    CONFIG_JSON=$(jq -c \
        --arg terminalUrl "$TERMINAL_URL" \
        --arg terminalToken "$TERMINAL_TOKEN" \
        '. + {
            enable_terminal_actions: true,
            terminal_action_url: $terminalUrl,
            terminal_action_token: $terminalToken
        }' <<<"$CONFIG_JSON")
fi

homeboy_datamachine_agent_wp_codebox_run

if [ -n "$TERMINAL_SERVER_PID" ]; then
    kill "$TERMINAL_SERVER_PID" >/dev/null 2>&1 || true
    TERMINAL_SERVER_PID=""
fi

if [ "$PRINT_RESULTS" = "1" ]; then
    cat "$RESULTS_FILE"
fi

scenario='.scenarios[] | select(.id == "'"$WORKLOAD_ID"'")'
if ! jq -e "$scenario" "$RESULTS_FILE" >/dev/null; then
    echo "ERROR: Data Machine agent workload did not run" >&2
    exit 1
fi

REPLAY_BUNDLE_DIR="${HOMEBOY_DATAMACHINE_AGENT_REPLAY_BUNDLE_DIR:-}"
if [ -z "$REPLAY_BUNDLE_DIR" ]; then
    REPLAY_BUNDLE_DIR=$(jq -r '.replay_bundle_dir // empty' "$CONFIG_PATH")
fi
if [ -n "$REPLAY_BUNDLE_DIR" ]; then
    if [ ! -f "$REPLAY_BUNDLE_BUILDER" ]; then
        echo "ERROR: replay bundle builder missing at $REPLAY_BUNDLE_BUILDER" >&2
        exit 1
    fi
    node "$REPLAY_BUNDLE_BUILDER" \
        --results "$RESULTS_FILE" \
        --scenario "$WORKLOAD_ID" \
        --config "$CONFIG_PATH" \
        --output-dir "$REPLAY_BUNDLE_DIR" \
        --update-results >/dev/null
fi

if jq -e "$scenario | .metadata.error?" "$RESULTS_FILE" >/dev/null; then
    echo "ERROR: Data Machine agent workload reported an error" >&2
    jq -r "$scenario | .metadata.error" "$RESULTS_FILE" >&2
    exit 1
fi

if ! jq -e "$scenario | .metrics.config_present_mean == 1" "$RESULTS_FILE" >/dev/null; then
    echo "ERROR: Data Machine agent workload returned an incomplete result" >&2
    exit 1
fi

# After this script returns, consumers can extract engine_data fields with:
#
#   "$EXTENSION_PATH/scripts/agent/extract-engine-data.sh" \
#       --results "$RESULTS_FILE" \
#       --scenario <scenario-id> \
#       --field key=metadata.engine_data.path.to.value
