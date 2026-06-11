#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
WORKLOAD_PATH="$SCRIPT_DIR/datamachine-agent-workload.php"
REPLAY_BUNDLE_BUILDER="$SCRIPT_DIR/build-replay-bundle.js"
WPGYM_EVAL_ROW_PROJECTOR="$SCRIPT_DIR/project-wpgym-eval-row.js"
WPGYM_EVAL_ROW_VALIDATOR="$SCRIPT_DIR/validate-wpgym-eval-row.js"

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

homeboy_datamachine_agent_wp_codebox_mount_type() {
    local source="${1:-}"
    local explicit_type="${2:-}"

    case "$explicit_type" in
        file|directory)
            printf '%s\n' "$explicit_type"
            return 0
            ;;
        "")
            ;;
        *)
            echo "ERROR: wp_codebox_mounts type must be file or directory: $explicit_type" >&2
            exit 1
            ;;
    esac

    if [ -f "$source" ]; then
        printf 'file\n'
        return 0
    fi
    if [ -d "$source" ]; then
        printf 'directory\n'
        return 0
    fi

    echo "ERROR: wp_codebox_mounts source must be an existing file or directory: $source" >&2
    exit 1
}

homeboy_datamachine_agent_add_wp_codebox_mount() {
    local source="${1:-}"
    local target="${2:-}"
    local mode="${3:-readwrite}"
    local type="${4:-}"

    if [ -z "$source" ] || [ -z "$target" ]; then
        return 0
    fi

    case "$mode" in
        readonly|readwrite)
            ;;
        *)
            echo "ERROR: wp_codebox_mounts mode must be readonly or readwrite: $mode" >&2
            exit 1
            ;;
    esac

    type="$(homeboy_datamachine_agent_wp_codebox_mount_type "$source" "$type")"
    mounts_json=$(jq -nc \
        --argjson mounts "$mounts_json" \
        --arg type "$type" \
        --arg source "$source" \
        --arg target "$target" \
        --arg mode "$mode" \
        '$mounts + [{type: $type, source: $source, target: $target, mode: $mode}]')
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
    local data_machine_path="${HOMEBOY_DATA_MACHINE_PATH:-${DATA_MACHINE_PATH:-}}"
    local data_machine_code_path="${HOMEBOY_DATA_MACHINE_CODE_PATH:-${DATA_MACHINE_CODE_PATH:-}}"
    if [ -z "$agents_api_path" ]; then
        agents_api_path=$(jq -r '.wp_codebox_components.agents_api // .wp_codebox_agents_api_path // empty' "$CONFIG_PATH")
    fi
    if [ -z "$data_machine_path" ]; then
        data_machine_path=$(jq -r '.wp_codebox_components.data_machine // .wp_codebox_data_machine_path // empty' "$CONFIG_PATH")
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
        --arg datamachine "$data_machine_path" \
        --arg code "$data_machine_code_path" \
        '[
            {source: $agents, slug: "agents-api", activate: false, loadAs: "mu-plugin"},
            {source: $datamachine, slug: "data-machine", activate: false, loadAs: "mu-plugin"},
            {source: $code, slug: "data-machine-code", activate: false, loadAs: "mu-plugin"}
        ]')
    mounts_json=$(jq -nc --arg source "$EXTENSION_PATH" '[{type: "directory", source: $source, target: "/homeboy-extension", mode: "readonly"}]')
    provider_slugs_csv=""

    if [ -n "$BUNDLE_PATH" ]; then
        homeboy_datamachine_agent_add_wp_codebox_mount "$BUNDLE_PATH" "$BUNDLE_GUEST_PATH" "readonly"
    fi

    local extra_mount
    while IFS= read -r extra_mount; do
        [ -n "$extra_mount" ] || continue
        if jq -e 'type == "string"' >/dev/null 2>&1 <<<"$extra_mount"; then
            IFS=: read -r mount_source mount_target mount_mode <<<"$(jq -r '.' <<<"$extra_mount")"
            homeboy_datamachine_agent_add_wp_codebox_mount "$mount_source" "$mount_target" "${mount_mode:-readwrite}"
            continue
        fi

        mount_source=$(jq -r '.source // .from // empty' <<<"$extra_mount")
        mount_target=$(jq -r '.target // .to // empty' <<<"$extra_mount")
        mount_mode=$(jq -r '.mode // "readwrite"' <<<"$extra_mount")
        mount_type=$(jq -r '.type // empty' <<<"$extra_mount")
        homeboy_datamachine_agent_add_wp_codebox_mount "$mount_source" "$mount_target" "$mount_mode" "$mount_type"
    done < <(jq -c '.wp_codebox_mounts? // [] | .[]?' <<<"$CONFIG_JSON")

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
        --arg wp "$WP_CODEBOX_WORDPRESS_VERSION" \
        --argjson extraPlugins "$extra_plugins_json" \
        --argjson mounts "$mounts_json" \
        --argjson secretEnv "$secret_env_json" \
        --arg task "$WORKLOAD_LABEL" \
        --arg codeFile "$workload_wrapper" \
        --arg providerSlugs "$provider_slugs_csv" \
        '{
            schema: "wp-codebox/workspace-recipe/v1",
            runtime: ({blueprint: {steps: []}} + (if $wp == "" then {} else {wp: $wp} end)),
            inputs: {extra_plugins: $extraPlugins, mounts: $mounts, secretEnv: $secretEnv},
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

    local wp_codebox_artifact_bundle
    wp_codebox_artifact_bundle=$(jq -r '.artifacts.directory // empty' "$wp_codebox_output")

    local artifact_verifier_result="$RUNTIME_DIR/wp-codebox-artifact-verifier.json"
    local workspace_policy_result="$RUNTIME_DIR/wp-codebox-workspace-policy.json"
    homeboy_datamachine_agent_run_artifact_verifier \
        "$wp_codebox_bin" \
        "$wp_codebox_artifact_bundle" \
        "$artifact_verifier_result"
    homeboy_datamachine_agent_run_workspace_policy_check \
        "$wp_codebox_bin" \
        "$wp_codebox_artifact_bundle" \
        "$workspace_policy_result"

    local wp_codebox_review_input="$RUNTIME_DIR/wp-codebox-review.json"
    local wp_codebox_changed_files_input="$RUNTIME_DIR/wp-codebox-changed-files.json"
    local wp_codebox_runtime_reference_manifest_input="$RUNTIME_DIR/wp-codebox-runtime-reference-manifest.json"
    local wp_codebox_transcript_input="$RUNTIME_DIR/wp-codebox-transcript.json"
    local wp_codebox_agent_result_input="$RUNTIME_DIR/wp-codebox-agent-result.json"
    local wp_codebox_completion_outcome_input="$RUNTIME_DIR/wp-codebox-completion-outcome.json"
    local wp_codebox_artifacts_json="$RUNTIME_DIR/wp-codebox-artifacts.json"
    printf 'null\n' >"$wp_codebox_review_input"
    printf 'null\n' >"$wp_codebox_changed_files_input"
    printf 'null\n' >"$wp_codebox_runtime_reference_manifest_input"
    printf 'null\n' >"$wp_codebox_transcript_input"
    printf 'null\n' >"$wp_codebox_agent_result_input"
    printf 'null\n' >"$wp_codebox_completion_outcome_input"

    local wp_codebox_review_path
    local wp_codebox_changed_files_path
    local wp_codebox_runtime_reference_manifest_path
    local wp_codebox_transcript_path
    local wp_codebox_agent_result_path
    local wp_codebox_completion_outcome_path
    wp_codebox_review_path=$(jq -r '.artifacts.reviewPath // empty' "$wp_codebox_output")
    wp_codebox_changed_files_path=$(jq -r '.artifacts.changedFilesPath // empty' "$wp_codebox_output")
    wp_codebox_runtime_reference_manifest_path=$(jq -r '
        [
            .artifacts.runtimeReferenceManifestPath?,
            .artifacts.runtimeReferencesManifestPath?,
            .artifacts.runtime_reference_manifest_path?,
            .artifacts.referenceManifestPath?,
            .artifacts.runtimeReferencePath?,
            .artifacts.runtimeReferencesPath?,
            .artifacts.runtime.referenceManifestPath?
        ] | map(select(type == "string" and . != "")) | first // empty
    ' "$wp_codebox_output")
    wp_codebox_transcript_path=$(jq -r '.artifacts.transcriptPath // .agentResult.transcript.artifact // empty' "$wp_codebox_output")
    wp_codebox_agent_result_path=$(jq -r '.artifacts.agentResultPath // empty' "$wp_codebox_output")
    wp_codebox_completion_outcome_path=$(jq -r '.artifacts.completionOutcomePath // empty' "$wp_codebox_output")
    if [ -n "$wp_codebox_artifact_bundle" ]; then
        case "$wp_codebox_transcript_path" in
            ""|/*) ;;
            *) wp_codebox_transcript_path="$wp_codebox_artifact_bundle/$wp_codebox_transcript_path" ;;
        esac
        case "$wp_codebox_agent_result_path" in
            ""|/*) ;;
            *) wp_codebox_agent_result_path="$wp_codebox_artifact_bundle/$wp_codebox_agent_result_path" ;;
        esac
        case "$wp_codebox_completion_outcome_path" in
            ""|/*) ;;
            *) wp_codebox_completion_outcome_path="$wp_codebox_artifact_bundle/$wp_codebox_completion_outcome_path" ;;
        esac
    fi
    if [ -z "$wp_codebox_transcript_path" ] && [ -n "$wp_codebox_artifact_bundle" ]; then
        wp_codebox_transcript_path="$wp_codebox_artifact_bundle/files/transcript.json"
    fi
    if [ -z "$wp_codebox_agent_result_path" ] && [ -n "$wp_codebox_artifact_bundle" ]; then
        wp_codebox_agent_result_path="$wp_codebox_artifact_bundle/files/agent-result.json"
    fi
    if [ -z "$wp_codebox_completion_outcome_path" ] && [ -n "$wp_codebox_artifact_bundle" ]; then
        wp_codebox_completion_outcome_path="$wp_codebox_artifact_bundle/files/completion-outcome.json"
    fi
    if [ -n "$wp_codebox_review_path" ] && [ -f "$wp_codebox_review_path" ]; then
        cp "$wp_codebox_review_path" "$wp_codebox_review_input"
    fi
    if [ -n "$wp_codebox_changed_files_path" ] && [ -f "$wp_codebox_changed_files_path" ]; then
        cp "$wp_codebox_changed_files_path" "$wp_codebox_changed_files_input"
    fi
    if [ -n "$wp_codebox_runtime_reference_manifest_path" ] && [ -f "$wp_codebox_runtime_reference_manifest_path" ]; then
        cp "$wp_codebox_runtime_reference_manifest_path" "$wp_codebox_runtime_reference_manifest_input"
    fi
    if [ -n "$wp_codebox_transcript_path" ] && [ -f "$wp_codebox_transcript_path" ]; then
        cp "$wp_codebox_transcript_path" "$wp_codebox_transcript_input"
    fi
    if [ -n "$wp_codebox_agent_result_path" ] && [ -f "$wp_codebox_agent_result_path" ]; then
        cp "$wp_codebox_agent_result_path" "$wp_codebox_agent_result_input"
    fi
    if [ -n "$wp_codebox_completion_outcome_path" ] && [ -f "$wp_codebox_completion_outcome_path" ]; then
        cp "$wp_codebox_completion_outcome_path" "$wp_codebox_completion_outcome_input"
    fi

    jq -n \
        --arg transcriptPath "$wp_codebox_transcript_path" \
        --slurpfile run "$wp_codebox_output" \
        --slurpfile review "$wp_codebox_review_input" \
        --slurpfile changedFiles "$wp_codebox_changed_files_input" \
        --slurpfile runtimeReferenceManifest "$wp_codebox_runtime_reference_manifest_input" \
        --slurpfile transcript "$wp_codebox_transcript_input" \
        --slurpfile agentResult "$wp_codebox_agent_result_input" \
        --slurpfile completionOutcome "$wp_codebox_completion_outcome_input" \
        --slurpfile verifier "$artifact_verifier_result" \
        --slurpfile policy "$workspace_policy_result" \
        '
        def secret_key($key): ($key | test("secret|token|password|passwd|authorization|cookie|nonce|api[_-]?key|access[_-]?key|private[_-]?key|bearer"; "i"));
        def redact:
            if type == "object" then with_entries(.value = if secret_key(.key) then "[redacted]" else (.value | redact) end)
            elif type == "array" then map(redact)
            elif type == "string" then gsub("(?i)(bearer|token|api[_-]?key|password|cookie|authorization|private[_-]?key)(\\s*[:=]\\s*)[^\\s,;]+"; "\\1\\2[redacted]")
            else . end;
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
                transcript: $transcriptPath,
                review: ($artifacts.reviewPath // ""),
                runtime_reference_manifest: ($artifacts.runtimeReferenceManifestPath // $artifacts.runtimeReferencesManifestPath // $artifacts.runtime_reference_manifest_path // $artifacts.referenceManifestPath // $artifacts.runtimeReferencePath // $artifacts.runtimeReferencesPath // $artifacts.runtime.referenceManifestPath // "")
            } | with_entries(select(.value != "")),
            review_payload: ($review[0] // null),
            changed_files: ($changedFiles[0] // null),
            runtime_reference_manifest: (($runtimeReferenceManifest[0] // null) | redact),
            transcript: ($transcript[0] // null),
            agent_result: ($agentResult[0] // null),
            completion_outcome: ($completionOutcome[0] // null),
            artifact_verifier_result: ($verifier[0] // null),
            workspace_policy_result: ($policy[0] // null)
        }
        ' >"$wp_codebox_artifacts_json"

    jq -n \
        --arg component "$COMPONENT_ID" \
        --arg workloadId "$WORKLOAD_ID" \
        --arg workloadLabel "$WORKLOAD_LABEL" \
        --slurpfile run "$wp_codebox_output" \
        --slurpfile wpCodeboxArtifacts "$wp_codebox_artifacts_json" \
        '
        def workload_from_record($record):
            if ($record | type) != "object" then {}
            elif (($record.metrics? // null) != null) or (($record.metadata? // null) != null) then $record
            elif ($record.output? | type) == "string" then (($record.output | fromjson? // {}) | workload_from_record(.))
            elif ($record.output? | type) == "object" then workload_from_record($record.output)
            elif ($record.result? | type) == "object" then workload_from_record($record.result)
            else {}
            end;
        def parsed_workload:
            ($wpCodeboxArtifacts[0].transcript // {}) as $transcript
            | [
                (($run[0].executions // [])[]?.stdout? | fromjson? // {}),
                ($run[0].agentTaskResult // {}),
                ($run[0].agentTaskResult.raw.result // {}),
                ($run[0].agentTaskResult.outputs // {}),
                ($wpCodeboxArtifacts[0].agent_result // {}),
                ($wpCodeboxArtifacts[0].completion_outcome // {}),
                ($transcript.executions[]?.parsed? // {})
            ]
            | map(workload_from_record(.))
            | map(select(((.metrics? // null) != null) or ((.metadata? // null) != null)))
            | first // {};
        def artifact_entry($path; $kind; $label):
            { path: $path, kind: $kind, label: $label };
        ($wpCodeboxArtifacts[0] // {}) as $wpArtifacts
        | parsed_workload as $parsedWorkload
        | (if (($parsedWorkload.metrics? // null) != null) or (($parsedWorkload.metadata? // null) != null) then
            $parsedWorkload
          elif (($run[0].success // false) == true) and (($wpArtifacts.artifact_verifier_result.success // false) == true) then
            {
              metrics: {config_present: 1},
              metadata: {
                success_status: "no_changes",
                completion_outcome_satisfied: false
              }
            }
          else
            {}
          end) as $workload
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
                        wp_codebox_patch: artifact_entry(($wpPaths.patch // ""); "patch"; "WP Codebox patch"),
                        wp_codebox_runtime_reference_manifest: artifact_entry(($wpPaths.runtime_reference_manifest // ""); "runtime-reference-manifest"; "WP Codebox runtime reference manifest")
                    } | with_entries(select(.value.path != ""))),
                    metadata: (($workload.metadata // {}) + {
                        wp_codebox: {
                            success: ($run[0].success // false),
                            runtime: ($run[0].runtime // null),
                            artifacts: ($run[0].artifacts // null),
                            canonical_artifacts: ($wpPaths // {}),
                            artifact_verifier_result: ($wpArtifacts.artifact_verifier_result // null),
                            workspace_policy_result: ($wpArtifacts.workspace_policy_result // null),
                            review_payload: ($wpArtifacts.review_payload // null),
                            changed_files: ($wpArtifacts.changed_files // null),
                            runtime_reference_manifest: ($wpArtifacts.runtime_reference_manifest // null),
                            error: ($run[0].error // null)
                        }
                    })
                }
            ]
        }
        ' >"$RESULTS_FILE"
}

homeboy_datamachine_agent_wp_codebox_command() {
    local wp_codebox_bin="$1"
    shift

    local wp_codebox_command=("$wp_codebox_bin")
    case "$wp_codebox_bin" in
        *.js)
            wp_codebox_command=(node "$wp_codebox_bin")
            ;;
    esac

    "${wp_codebox_command[@]}" "$@"
}

homeboy_datamachine_agent_write_check_result() {
    local output_path="$1"
    local check_name="$2"
    local required="$3"
    local skipped_reason="$4"
    local exit_code="$5"
    local stdout_path="$6"

    jq -n \
        --arg check "$check_name" \
        --argjson required "$required" \
        --arg skippedReason "$skipped_reason" \
        --argjson exitCode "$exit_code" \
        --rawfile stdout "$stdout_path" \
        '
        ($stdout | fromjson? // null) as $jsonOutput
        | {
            schema: "homeboy/wp-codebox-post-run-check/v1",
            check: $check,
            required: $required,
            skipped: ($skippedReason != ""),
            skipped_reason: $skippedReason,
            exit_code: $exitCode,
            success: (($skippedReason == "") and ($exitCode == 0) and (($jsonOutput.success // $jsonOutput.valid // true) == true)),
            output: $jsonOutput,
            stdout: (if $jsonOutput == null then $stdout else null end)
        }
        | with_entries(select(.value != null and .value != ""))
        ' >"$output_path"
}

homeboy_datamachine_agent_run_artifact_verifier() {
    local wp_codebox_bin="$1"
    local artifact_bundle="$2"
    local result_path="$3"
    local stdout_path="$RUNTIME_DIR/wp-codebox-artifact-verifier.stdout"
    local required

    required=$(jq -r 'if (.require_artifact_verifier // .wp_codebox_require_artifact_verifier // true) then "true" else "false" end' <<<"$CONFIG_JSON")
    printf '' >"$stdout_path"

    if [ -z "$artifact_bundle" ]; then
        homeboy_datamachine_agent_write_check_result "$result_path" "artifact_verifier" "$required" "artifact_bundle_missing" 1 "$stdout_path"
    else
        set +e
        homeboy_datamachine_agent_wp_codebox_command "$wp_codebox_bin" artifacts verify --artifacts "$artifact_bundle" --json >"$stdout_path" 2>&1
        local verifier_exit=$?
        set -e
        homeboy_datamachine_agent_write_check_result "$result_path" "artifact_verifier" "$required" "" "$verifier_exit" "$stdout_path"
    fi

    if [ "$required" = "true" ] && ! jq -e '.success == true' "$result_path" >/dev/null; then
        echo "ERROR: required WP Codebox artifact verification failed or was unavailable" >&2
        cat "$result_path" >&2
        exit 1
    fi
}

homeboy_datamachine_agent_workspace_policy_args_json() {
    jq -c '
        def string_array($value):
            if ($value | type) == "array" then [$value[] | select(type == "string" and . != "")]
            elif ($value | type) == "string" and $value != "" then [$value]
            else [] end;
        (.workspace_policy_check // {}) as $check
        | string_array($check.args // .workspace_policy_args // [])
    ' <<<"$CONFIG_JSON"
}

homeboy_datamachine_agent_workspace_policy_available() {
    jq -e '
        (.workspace_policy_check // {}) as $check
        | (($check.enabled // false) == true)
            or (($check.args // .workspace_policy_args // []) | if type == "array" then length > 0 elif type == "string" then . != "" else false end)
            or ((.workspace_policy_input // .workspace_policy_path // .workspace_policy // null) != null)
    ' <<<"$CONFIG_JSON" >/dev/null
}

homeboy_datamachine_agent_run_workspace_policy_check() {
    local wp_codebox_bin="$1"
    local artifact_bundle="$2"
    local result_path="$3"
    local stdout_path="$RUNTIME_DIR/wp-codebox-workspace-policy.stdout"
    local input_path="$RUNTIME_DIR/wp-codebox-workspace-policy-input.json"
    local required

    required=$(jq -r 'if (.require_workspace_policy // .wp_codebox_require_workspace_policy // false) then "true" else "false" end' <<<"$CONFIG_JSON")
    printf '' >"$stdout_path"

    if ! homeboy_datamachine_agent_workspace_policy_available; then
        homeboy_datamachine_agent_write_check_result "$result_path" "workspace_policy" "$required" "policy_inputs_missing" 1 "$stdout_path"
    else
        jq '
            {
                workspace_policy_input: (.workspace_policy_input // null),
                workspace_policy_path: (.workspace_policy_path // null),
                workspace_policy: (.workspace_policy // null),
                runner_workspace: (.runner_workspace // null),
                runner_workspace_result: (.runner_workspace_result // null),
                artifact_bundle: $artifactBundle
            }
        ' --arg artifactBundle "$artifact_bundle" <<<"$CONFIG_JSON" >"$input_path"

        local policy_args_json
        policy_args_json=$(homeboy_datamachine_agent_workspace_policy_args_json)
        local policy_args=(workspace-policy check --json --input "$input_path")
        if [ -n "$artifact_bundle" ]; then
            policy_args+=(--artifacts "$artifact_bundle")
        fi
        while IFS= read -r policy_arg; do
            [ -n "$policy_arg" ] || continue
            policy_args+=("$policy_arg")
        done < <(jq -r '.[]' <<<"$policy_args_json")

        set +e
        homeboy_datamachine_agent_wp_codebox_command "$wp_codebox_bin" "${policy_args[@]}" >"$stdout_path" 2>&1
        local policy_exit=$?
        set -e
        homeboy_datamachine_agent_write_check_result "$result_path" "workspace_policy" "$required" "" "$policy_exit" "$stdout_path"
    fi

    if [ "$required" = "true" ] && ! jq -e '.success == true' "$result_path" >/dev/null; then
        echo "ERROR: required WP Codebox workspace policy check failed or was unavailable" >&2
        cat "$result_path" >&2
        exit 1
    fi
}

homeboy_datamachine_agent_attach_evidence_references() {
    local workflow_run_url=""
    if [ -n "${GITHUB_REPOSITORY:-}" ] && [ -n "${GITHUB_RUN_ID:-}" ]; then
        workflow_run_url="${GITHUB_SERVER_URL:-https://github.com}/${GITHUB_REPOSITORY}/actions/runs/${GITHUB_RUN_ID}"
    fi

    local updated_results
    updated_results=$(mktemp "${TMPDIR:-/tmp}/homeboy-datamachine-agent-evidence.XXXXXX")
    jq \
        --arg scenarioId "$WORKLOAD_ID" \
        --arg resultsPath "$RESULTS_FILE" \
        --arg workflowRunUrl "$workflow_run_url" \
        --argjson config "$CONFIG_JSON" \
        '
        def present($value): $value != null and $value != "" and $value != [] and $value != {};
        def ref($kind; $path; $label; $source):
            {kind: $kind, path: $path, label: $label, source: $source, available: present($path)};
        def inline_ref($kind; $value; $label; $source):
            {kind: $kind, value: $value, label: $label, source: $source, available: present($value)};
        def gap($field; $reason):
            {field: $field, reason: $reason, compatibility_gap: true};
        def secret_key($key): ($key | test("secret|token|password|passwd|authorization|cookie|nonce|api[_-]?key|access[_-]?key|private[_-]?key|bearer"; "i"));
        def redact:
            if type == "object" then with_entries(.value = if secret_key(.key) then "[redacted]" else (.value | redact) end)
            elif type == "array" then map(redact)
            elif type == "string" then gsub("(?i)(bearer|token|api[_-]?key|password|cookie|authorization|private[_-]?key)(\\s*[:=]\\s*)[^\\s,;]+"; "\\1\\2[redacted]")
            else . end;
        def tool_summary($metadata):
            ([
                ($metadata.tool_audit_events // [])[]?,
                ($metadata.engine_data.tool_execution_summary // [])[]?,
                ($metadata.engine_data.tool_results // [])[]?,
                ($metadata.engine_data.github_tool_results // [])[]?
            ] | map(select(type == "object")) | map({
                tool_name: (.tool_name // .action_name // .name // ""),
                tool_source: (.tool_source // .source // ""),
                success: (.success // false),
                result_status: (.result_status // (if (.success // false) then "success" else "unknown" end)),
                parameters_redacted: (.parameters_redacted // true),
                parameters_sha256: (.parameters_sha256 // .args_sha256 // ""),
                result_sha256: (.result_sha256 // ""),
                error_type: (.error_type // "")
            } | with_entries(select(.value != "")))) as $events
            | {
                count: ($events | length),
                names: ($events | map(.tool_name) | map(select(. != "")) | unique),
                events: $events
            };
        def runner_publications($metadata):
            [
                ($metadata.engine_data.runner_publications // [])[]?,
                ($metadata.engine_data[]? | objects | .runner_publications? // [] | .[]?)
            ];
        def first_runner_publication_pr_url($metadata):
            (runner_publications($metadata) | map(select(.tool_name == "create_github_pull_request" and .success == true and (.url // "") != "") | .url) | first) // "";
        def runner_evidence($scenario):
            ($scenario.metadata // {}) as $metadata
            | ($metadata.wp_codebox.canonical_artifacts // {}) as $wpPaths
            | ($metadata.runner_workspace_capture // {}) as $workspaceCapture
            | ($workspaceCapture.status // {}) as $workspaceStatus
            | {
                schema: "homeboy/datamachine-agent-runner-evidence/v1",
                redaction: {
                    applied: true,
                    marker: "[redacted]",
                    policy: "key-name and inline secret marker redaction for prompt/tool/workspace/runtime evidence"
                },
                prompt_surface: {
                    sha256: ($metadata.fingerprints.prompt.sha256 // ""),
                    bytes: ($metadata.fingerprints.prompt.bytes // (($metadata.prompt // "") | length)),
                    prompt: (($metadata.prompt // "") | redact),
                    prompt_env: (($config.prompt_env // "") | redact),
                    instruction_sources: ([
                        {kind: "bundle", path: ($metadata.bundle_path // $config.bundle_path // "")},
                        {kind: "flow", slug: ($metadata.flow_slug // $config.flow_slug // "")},
                        {kind: "agent", slug: ($metadata.agent_slug // $config.agent_slug // "")}
                    ] | map(select(.path != "" or .slug != "")))
                },
                tool_surface: {
                    required_abilities: (($config.required_abilities // []) | redact),
                    ability_tools: (($config.ability_tools // []) | redact),
                    tool_recorders: (($config.tool_recorders // []) | redact),
                    audit_summary: tool_summary($metadata)
                },
                workspace_surface: {
                    configured: (($config.runner_workspace // {}) | redact),
                    provisioned: (($metadata.runner_workspace // {}) | redact),
                    captured: {
                        enabled: ($workspaceCapture.enabled // false),
                        changed: ($workspaceCapture.changed // false),
                        handle: ($workspaceStatus.handle // $workspaceStatus.name // ""),
                        branch: ($workspaceStatus.branch // ""),
                        dirty: ($workspaceStatus.dirty // null),
                        files: (($workspaceStatus.files // []) | redact),
                        diff_redacted: (if present($workspaceCapture.diff) then true else false end)
                    } | with_entries(select(.value != null and .value != "" and .value != []))
                },
                publication_surface: {
                    runner_publications: (runner_publications($metadata) | redact),
                    pr_url: (first_runner_publication_pr_url($metadata) | redact)
                } | with_entries(select(.value != null and .value != "" and .value != [])),
                runtime_surface: {
                    provider: ($metadata.provider // $config.provider // ""),
                    model: ($metadata.model // $config.model // ""),
                    runtime_versions: (($metadata.runtime_versions // {}) | redact),
                    wp_codebox_runtime: (($metadata.wp_codebox.runtime // null) | redact),
                    wp_codebox_paths: ($wpPaths | redact),
                    runtime_reference_manifest_available: present($metadata.wp_codebox.runtime_reference_manifest)
                } | with_entries(select(.value != null and .value != "" and .value != {})),
                config_surface: ({
                    component_id: ($config.component_id // ""),
                    workload_id: ($config.workload_id // ""),
                    workload_label: ($config.workload_label // ""),
                    dry_run: ($config.dry_run // false),
                    secret_env_names: (($config.secret_env // $config.secretEnv // []) | if type == "array" then . else [] end)
                } | redact | with_entries(select(.value != "" and .value != [])))
            };
        def first_field($source; $name):
            ([$source | .. | objects | .[$name]? | select(present(.))] | first) // null;
        def first_tool_pr_url($metadata):
            ([
                ($metadata.engine_data.github_tool_results // [])[]?,
                ($metadata.engine_data[]? | objects | .github_tool_results? // [] | .[]?)
            ] | map(select(.tool_name == "create_github_pull_request" and .success == true and (.url // "") != "") | .url) | first) // "";
        def pr_url($metadata):
            first_field($metadata.job_artifact_exports; "pr_url")
            // first_field($metadata.fallback_pull_request; "html_url")
            // first_field($metadata.fallback_pull_request; "url")
            // first_runner_publication_pr_url($metadata)
            // first_tool_pr_url($metadata)
            // "";
        def workspace_branch($metadata):
            first_field($metadata.runner_workspace_capture; "branch")
            // first_field($metadata.runner_workspace; "branch")
            // first_field($metadata.job_artifact_exports; "branch")
            // first_field($metadata.fallback_pull_request; "head")
            // "";
        def eval_workflow_run_url($metadata):
            if ($metadata.eval_artifact.run? | type) == "object" then
                ($metadata.eval_artifact.run.workflow_run_url // "")
            elif ($metadata.eval_artifact.run? | type) == "array" then
                ([$metadata.eval_artifact.run[]? | objects | .workflow_run_url? | select(present(.))] | first) // ""
            else
                ""
            end;
        def evidence($scenario):
            ($scenario.metadata // {}) as $metadata
            | ($scenario.artifacts // {}) as $artifacts
            | ($metadata.wp_codebox.canonical_artifacts // {}) as $wpPaths
            | ($metadata.fingerprints // {}) as $fingerprints
            | ($metadata.runtime_versions // {}) as $runtimeVersions
            | first_field($metadata; "artifact_verifier_result") as $artifactVerifier
            | (first_field($metadata; "workspace_policy_result") // $metadata.datamachine_code_policy_attestation // null) as $policyResult
            | ($artifacts.episode_jsonl.path // $metadata.runtime_episode_trace.path // $metadata.runtime_episode_trace // "") as $episodeTrace
            | ($artifacts.replay_bundle.path // $metadata.replay_bundle_path // "") as $replayBundle
            | ($wpPaths.transcript // "") as $transcriptJson
            | {
                schema: "homeboy/datamachine-agent-evidence-references/v1",
                scope: "generic-runner-evidence",
                references: {
                    homeboy_result_json: ref("json"; $resultsPath; "Homeboy result JSON"; "homeboy"),
                    wp_codebox_artifact_bundle: ref("directory"; ($wpPaths.directory // ""); "WP Codebox artifact bundle"; "wp-codebox"),
                    wp_codebox_manifest: ref("json"; ($wpPaths.manifest // ""); "WP Codebox manifest"; "wp-codebox"),
                    wp_codebox_runtime_reference_manifest: ref("json"; ($wpPaths.runtime_reference_manifest // ""); "WP Codebox runtime reference manifest"; "wp-codebox"),
                    artifact_verifier_result: inline_ref("json"; $artifactVerifier; "Artifact verifier result"; "runner"),
                    workspace_policy_result: inline_ref("json"; $policyResult; "Workspace policy result"; "data-machine-code"),
                    runtime_episode_trace: ref("jsonl"; $episodeTrace; "Runtime episode trace"; "homeboy"),
                    transcript_artifact: ref("json"; $transcriptJson; "Transcript artifact"; "data-machine"),
                    replay_bundle_artifact: ref("json"; $replayBundle; "Replay bundle artifact"; "homeboy"),
                    workspace_diff: inline_ref("diff"; ($metadata.runner_workspace_capture.diff // null); "Workspace diff"; "data-machine-code"),
                    workspace_changed_files: inline_ref("json"; ($metadata.runner_workspace_capture.status.files // $metadata.wp_codebox.changed_files // null); "Workspace changed files"; "runner"),
                    grader_result: inline_ref("json"; ($metadata.eval_artifact.grade // $metadata.grade // null); "Grader result"; "runner"),
                    pull_request: inline_ref("url"; pr_url($metadata); "Pull request URL"; "github"),
                    workspace_branch: inline_ref("git-ref"; workspace_branch($metadata); "Workspace branch"; "runner"),
                    workflow_run: ref("url"; ($metadata.workflow_run_url // eval_workflow_run_url($metadata) // $workflowRunUrl); "Workflow run"; "github")
                },
                fingerprints: {
                    prompt_sha256: ($fingerprints.prompt.sha256 // ""),
                    bundle_sha256: ($fingerprints.bundle.sha256 // ""),
                    tool_policy_sha256: ($fingerprints.tool_policy.sha256 // ""),
                    provider: ($metadata.provider // ""),
                    model: ($metadata.model // ""),
                    runtime: ($metadata.wp_codebox.runtime // null),
                    versions: $runtimeVersions
                },
                compatibility_gaps: ([
                    if present($artifactVerifier) then empty else gap("artifact_verifier_result"; "No artifact verifier result was supplied by this run.") end,
                    if present($policyResult) then empty else gap("workspace_policy_result"; "No workspace policy result was supplied by Data Machine Code.") end,
                    if present($episodeTrace) then empty else gap("runtime_episode_trace"; "No episode trace artifact was generated for this run.") end,
                    if present($transcriptJson) then empty else gap("transcript_artifact"; "No transcript artifact path was available for this run.") end,
                    if present($replayBundle) then empty else gap("replay_bundle_artifact"; "No replay bundle artifact was generated for this run.") end
                ])
            };
        .scenarios |= map(if .id == $scenarioId then .metadata.runner_evidence = runner_evidence(.) | .metadata.evidence_references = evidence(.) else . end)
        ' "$RESULTS_FILE" >"$updated_results"
    mv "$updated_results" "$RESULTS_FILE"
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
WP_CODEBOX_WORDPRESS_VERSION=$(jq -r '.wp_codebox_wordpress_version // ""' "$CONFIG_PATH")
ENABLE_TERMINAL_ACTIONS=$(jq -r 'if (.enable_terminal_actions // .enable_wp_cli_tool // false) then "1" else "0" end' <<<"$CONFIG_JSON")
if [ "$ENABLE_TERMINAL_ACTIONS" = "1" ]; then
    if [ -z "$RUNTIME_DIR" ]; then
        RUNTIME_DIR=$(mktemp -d "${TMPDIR:-/tmp}/homeboy-datamachine-agent-runtime.XXXXXX")
    fi
    TERMINAL_READY_FILE="$RUNTIME_DIR/terminal-action-server.json"
    TERMINAL_TOKEN="$(node -e "process.stdout.write(require('node:crypto').randomBytes(24).toString('base64url'))")"
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

homeboy_datamachine_agent_attach_evidence_references

if [ -f "$WPGYM_EVAL_ROW_PROJECTOR" ]; then
    wpgym_projector_args=(
        --results "$RESULTS_FILE"
        --scenario "$WORKLOAD_ID"
        --config "$CONFIG_PATH"
        --update-results
    )
    if [ "$(jq -r 'if (.wp_gym_eval.benchmark_mode // false) then "1" else "0" end' "$CONFIG_PATH")" = "1" ]; then
        wpgym_projector_args+=(--benchmark-mode)
    fi
    node "$WPGYM_EVAL_ROW_PROJECTOR" "${wpgym_projector_args[@]}" >/dev/null

    if [ "$(jq -r 'if (.wp_gym_eval.benchmark_mode // false) then "1" else "0" end' "$CONFIG_PATH")" = "1" ]; then
        if [ ! -f "$WPGYM_EVAL_ROW_VALIDATOR" ]; then
            echo "ERROR: wp-gym eval row validator missing at $WPGYM_EVAL_ROW_VALIDATOR" >&2
            exit 1
        fi
        node "$WPGYM_EVAL_ROW_VALIDATOR" \
            --results "$RESULTS_FILE" \
            --scenario "$WORKLOAD_ID" \
            --config "$CONFIG_PATH" >/dev/null
    fi
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
