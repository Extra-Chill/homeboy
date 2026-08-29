#!/usr/bin/env bash
set -euo pipefail

# Node.js fuzz runner for `homeboy fuzz`.
#
# Execution order:
#   1. HOMEBOY_NODE_FUZZ_COMMAND full command override.
#   2. HOMEBOY_FUZZ_WORKLOAD_PATH script file or JSON descriptor from a rig.
#   3. Configured package script (HOMEBOY_NODE_FUZZ_SCRIPT / settings.fuzz_script).
#   4. package.json scripts.fuzz.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
SETTINGS_HELPER="${HOMEBOY_RUNTIME_SETTINGS_HELPER:?Homeboy core must provide HOMEBOY_RUNTIME_SETTINGS_HELPER}"

BASH_PREFLIGHT_HELPER="${HOMEBOY_RUNTIME_BASH_PREFLIGHT:?Homeboy core must provide HOMEBOY_RUNTIME_BASH_PREFLIGHT}"
# shellcheck source=/dev/null
source "$BASH_PREFLIGHT_HELPER"
homeboy_require_bash_version 4

RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:?HOMEBOY_RUNTIME_RESOLVE_CONTEXT is required}"
# shellcheck source=/dev/null
source "$RESOLVE_CONTEXT_HELPER"
homeboy_resolve_context
# shellcheck source=/dev/null
source "$SETTINGS_HELPER"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/../lib/node-helpers.sh"
homeboy_require_package_json
homeboy_detect_package_manager

RESULTS_FILE="${HOMEBOY_FUZZ_RESULTS_FILE:?HOMEBOY_FUZZ_RESULTS_FILE is required}"
ARTIFACTS_DIR="${HOMEBOY_FUZZ_ARTIFACTS_DIR:-$(dirname "$RESULTS_FILE")/artifacts}"
RUNNER_ARGS=("$@")

mkdir -p "$(dirname "$RESULTS_FILE")" "$ARTIFACTS_DIR"
export HOMEBOY_FUZZ_RESULTS_FILE="$RESULTS_FILE"
export HOMEBOY_FUZZ_ARTIFACTS_DIR="$ARTIFACTS_DIR"
export HOMEBOY_COMPONENT_ID="$COMPONENT_ID"
export HOMEBOY_COMPONENT_PATH="$PROJECT_PATH"

homeboy_node_fuzz_script_runner() {
    local workload_path="$1"
    case "$workload_path" in
        *.ts|*.tsx)
            if [ -x "${PROJECT_PATH}/node_modules/.bin/tsx" ]; then
                printf '%s' "${PROJECT_PATH}/node_modules/.bin/tsx"
            elif command -v tsx >/dev/null 2>&1; then
                printf '%s' "tsx"
            elif command -v npx >/dev/null 2>&1; then
                printf '%s' "npx --yes tsx"
            else
                echo "Error: TypeScript fuzz workload '${workload_path}' requires tsx in the project, PATH, or npx." >&2
                return 1
            fi
            ;;
        *)
            printf '%s' "node"
            ;;
    esac
}

homeboy_node_run_script_workload() {
    local workload_path="$1"
    local runner
    runner="$(homeboy_node_fuzz_script_runner "$workload_path")"
    # shellcheck disable=SC2086 # word-splitting is intentional for npx --yes tsx.
    $runner "$workload_path" "${RUNNER_ARGS[@]}"
}

homeboy_node_run_json_workload() {
    local workload_path="$1"
    local descriptor_file command_path script_name command
    descriptor_file="$(mktemp "${TMPDIR:-/tmp}/homeboy-node-fuzz-descriptor.XXXXXX")"
    trap 'rm -f "$descriptor_file"' RETURN

    node --input-type=module - "$workload_path" > "$descriptor_file" <<'EOF'
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

const workloadPath = process.argv[2];
const workload = JSON.parse(readFileSync(workloadPath, 'utf8'));
const nestedWorkload = workload && typeof workload.workload === 'object' && workload.workload !== null ? workload.workload : {};
const stringField = (name) => {
  const nestedValue = nestedWorkload[name];
  if (typeof nestedValue === 'string' && nestedValue.trim()) return nestedValue.trim();
  const topLevelValue = workload[name];
  return typeof topLevelValue === 'string' ? topLevelValue.trim() : '';
};
const rawArgs = Array.isArray(nestedWorkload.args) ? nestedWorkload.args : workload.args;
const args = Array.isArray(rawArgs) ? rawArgs.map(String) : [];
const path = stringField('path');
const value = {
  command: stringField('command'),
  script: stringField('script'),
  path: path ? resolve(process.cwd(), path) : '',
  args,
};
process.stdout.write(JSON.stringify(value));
EOF

    command="$(node -e "const fs=require('fs'); const value=JSON.parse(fs.readFileSync(process.argv[1], 'utf8')); console.log(value.command || '')" "$descriptor_file")"
    script_name="$(node -e "const fs=require('fs'); const value=JSON.parse(fs.readFileSync(process.argv[1], 'utf8')); console.log(value.script || '')" "$descriptor_file")"
    command_path="$(node -e "const fs=require('fs'); const value=JSON.parse(fs.readFileSync(process.argv[1], 'utf8')); console.log(value.path || '')" "$descriptor_file")"
    mapfile -t DESCRIPTOR_ARGS < <(node -e "const fs=require('fs'); const value=JSON.parse(fs.readFileSync(process.argv[1], 'utf8')); for (const arg of value.args || []) console.log(arg)" "$descriptor_file")
    RUNNER_ARGS=("${DESCRIPTOR_ARGS[@]}" "${RUNNER_ARGS[@]}")

    if [ -n "$command" ]; then
        bash -c "$command \"\$@\"" _ "${RUNNER_ARGS[@]}"
    elif [ -n "$script_name" ]; then
        if ! homeboy_has_npm_script "$script_name"; then
            echo "Error: Node.js fuzz script '${script_name}' from ${workload_path} is not defined in package.json" >&2
            return 1
        fi
        command="$(homeboy_project_run_script_command "$script_name")"
        bash -c "$command -- \"\$@\"" _ "${RUNNER_ARGS[@]}"
    elif [ -n "$command_path" ]; then
        if [ ! -f "$command_path" ]; then
            echo "Error: Node.js fuzz workload path '${command_path}' from ${workload_path} does not exist" >&2
            return 1
        fi
        homeboy_node_run_script_workload "$command_path"
    else
        echo "Error: Node.js fuzz workload JSON '${workload_path}' must define command, script, or path" >&2
        return 1
    fi
}

homeboy_node_configured_fuzz_script() {
    local configured_script="${HOMEBOY_NODE_FUZZ_SCRIPT:-}"
    if [ -z "$configured_script" ]; then
        configured_script="$(homeboy_setting node_fuzz_script '.node_fuzz_script // .fuzz_script // .fuzz.script // empty')"
    fi
    if [ -n "$configured_script" ]; then
        printf '%s' "$configured_script"
        return 0
    fi
    if homeboy_has_npm_script "fuzz"; then
        printf '%s' "fuzz"
        return 0
    fi
    return 1
}

echo "Running Node.js fuzz..."
echo "  Component: ${COMPONENT_ID} (${PROJECT_PATH})"
echo "  Results:   ${RESULTS_FILE}"

cd "$PROJECT_PATH"

if [ -n "${HOMEBOY_NODE_FUZZ_COMMAND:-}" ]; then
    echo "  Command:   ${HOMEBOY_NODE_FUZZ_COMMAND}"
    bash -c "$HOMEBOY_NODE_FUZZ_COMMAND \"\$@\"" _ "${RUNNER_ARGS[@]}"
elif [ -n "${HOMEBOY_FUZZ_WORKLOAD_PATH:-}" ]; then
    WORKLOAD_PATH="$HOMEBOY_FUZZ_WORKLOAD_PATH"
    if [ ! -f "$WORKLOAD_PATH" ]; then
        echo "Error: Node.js fuzz workload not found at ${WORKLOAD_PATH}" >&2
        exit 2
    fi
    echo "  Workload:  ${WORKLOAD_PATH}"
    case "$WORKLOAD_PATH" in
        *.json)
            homeboy_node_run_json_workload "$WORKLOAD_PATH"
            ;;
        *)
            homeboy_node_run_script_workload "$WORKLOAD_PATH"
            ;;
    esac
elif FUZZ_SCRIPT="$(homeboy_node_configured_fuzz_script)"; then
    if ! homeboy_has_npm_script "$FUZZ_SCRIPT"; then
        echo "Error: configured Node.js fuzz script '${FUZZ_SCRIPT}' is not defined in package.json" >&2
        exit 1
    fi
    FUZZ_CMD="$(homeboy_project_run_script_command "$FUZZ_SCRIPT")"
    echo "  Script:    ${FUZZ_SCRIPT}"
    bash -c "$FUZZ_CMD -- \"\$@\"" _ "${RUNNER_ARGS[@]}"
else
    echo "Error: No Node.js fuzz workload or package script configured." >&2
    echo "Declare a rig fuzz workload path, set nodejs.fuzz_script/HOMEBOY_NODE_FUZZ_SCRIPT, or add package.json scripts.fuzz." >&2
    exit 1
fi

if [ ! -f "$RESULTS_FILE" ]; then
    echo "Error: Node.js fuzz completed without writing ${RESULTS_FILE}" >&2
    exit 1
fi

echo ""
echo "Node.js fuzz run complete."
