#!/usr/bin/env bash
set -euo pipefail

# Homeboy agent-task promotion provider backed by Data Machine Code workspaces.
# Reads homeboy/agent-task-promotion-apply-request/v1 JSON on stdin and writes
# homeboy/agent-task-promotion-apply-response/v1 JSON on stdout.

REQUEST_SCHEMA="homeboy/agent-task-promotion-apply-request/v1"
RESPONSE_SCHEMA="homeboy/agent-task-promotion-apply-response/v1"

require_tool() {
    local tool="$1"
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "Missing required tool: $tool" >&2
        exit 127
    fi
}

json_command_array() {
    local first=1
    printf '['
    local arg
    for arg in "$@"; do
        if [ "$first" -eq 1 ]; then
            first=0
        else
            printf ','
        fi
        printf '%s' "$arg" | jq -Rsa .
    done
    printf ']'
}

append_evidence() {
    local evidence_file="$1"
    local command_json="$2"
    local exit_code="$3"
    local stdout_file="$4"
    local stderr_file="$5"
    local entry_file
    entry_file="$(mktemp)"
    jq -n \
        --argjson command "$command_json" \
        --argjson exit_code "$exit_code" \
        --rawfile stdout "$stdout_file" \
        --rawfile stderr "$stderr_file" \
        '{command:$command,exit_code:$exit_code,stdout:$stdout,stderr:$stderr}' >"$entry_file"
    jq -s '.[0] + [.[1]]' "$evidence_file" "$entry_file" >"${evidence_file}.next"
    mv "${evidence_file}.next" "$evidence_file"
    rm -f "$entry_file"
}

run_dmc_command() {
    local evidence_file="$1"
    local capture="$2"
    shift 2
    local stdout_file stderr_file command_json exit_code
    stdout_file="$(mktemp)"
    stderr_file="$(mktemp)"
    command_json="$(json_command_array "$@")"
    if "$@" >"$stdout_file" 2>"$stderr_file"; then
        exit_code=0
    else
        exit_code=$?
    fi
    if [ "$capture" = "yes" ]; then
        append_evidence "$evidence_file" "$command_json" "$exit_code" "$stdout_file" "$stderr_file"
    fi
    if [ "$exit_code" -ne 0 ]; then
        cat "$stderr_file" >&2
        rm -f "$stdout_file" "$stderr_file"
        exit "$exit_code"
    fi
    cat "$stdout_file"
    rm -f "$stdout_file" "$stderr_file"
}

worktree_path_from_list() {
    local rows="$1"
    local handle="$2"
    jq -r --arg handle "$handle" '
        if type != "array" then empty
        else (.[] | select(.handle == $handle) | .path) // empty
        end
    ' <<<"$rows"
}

require_tool jq

REQUEST_JSON="$(cat)"
schema="$(jq -r '.schema // empty' <<<"$REQUEST_JSON")"
if [ -n "$schema" ] && [ "$schema" != "$REQUEST_SCHEMA" ]; then
    echo "Expected request schema $REQUEST_SCHEMA, got $schema" >&2
    exit 64
fi

to_workspace="$(jq -r '.to_workspace // empty' <<<"$REQUEST_JSON")"
patch_path="$(jq -r '.patch_path // empty' <<<"$REQUEST_JSON")"
if [ -z "$to_workspace" ]; then
    echo "Request must include to_workspace" >&2
    exit 64
fi
if [ -z "$patch_path" ]; then
    echo "Request must include patch_path" >&2
    exit 64
fi
if [ ! -f "$patch_path" ]; then
    echo "Patch path does not exist: $patch_path" >&2
    exit 66
fi
if [[ "$to_workspace" != *@* ]]; then
    echo "Data Machine Code promotion provider expects to_workspace as <repo>@<branch-slug>" >&2
    exit 64
fi

repo="${to_workspace%@*}"
branch="${to_workspace#*@}"
if [ -z "$repo" ] || [ -z "$branch" ]; then
    echo "Data Machine Code promotion provider expects non-empty repo and branch in to_workspace" >&2
    exit 64
fi

EVIDENCE_FILE="$(mktemp)"
trap 'rm -f "$EVIDENCE_FILE"' EXIT
printf '[]\n' >"$EVIDENCE_FILE"

list_output="$(run_dmc_command "$EVIDENCE_FILE" no studio wp datamachine-code workspace worktree list "$repo" --format=json)"
workspace_path="$(worktree_path_from_list "$list_output" "$to_workspace")"

if [ -z "$workspace_path" ]; then
    run_dmc_command "$EVIDENCE_FILE" yes studio wp datamachine-code workspace worktree add "$repo" "$branch" >/dev/null
    list_output="$(run_dmc_command "$EVIDENCE_FILE" no studio wp datamachine-code workspace worktree list "$repo" --format=json)"
    workspace_path="$(worktree_path_from_list "$list_output" "$to_workspace")"
fi

if [ -z "$workspace_path" ]; then
    echo "Managed workspace $to_workspace was not found after creation" >&2
    exit 1
fi

run_dmc_command "$EVIDENCE_FILE" yes studio wp datamachine-code workspace patch apply "$to_workspace" "--patch=@${patch_path}" --format=json >/dev/null

jq -n \
    --arg schema "$RESPONSE_SCHEMA" \
    --arg workspace_path "$workspace_path" \
    --slurpfile command_evidence "$EVIDENCE_FILE" \
    '{schema:$schema,workspace_path:$workspace_path,command_evidence:$command_evidence[0]}'
