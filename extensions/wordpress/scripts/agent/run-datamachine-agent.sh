#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
WORKLOAD_PATH="$SCRIPT_DIR/datamachine-agent-workload.php"
WORKLOAD_PLAYGROUND_PATH="/homeboy-extension/scripts/agent/datamachine-agent-workload.php"

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

RESULTS_FILE="${HOMEBOY_DATAMACHINE_AGENT_RESULTS_FILE:-${HOMEBOY_BENCH_RESULTS_FILE:-}}"
if [ -z "$RESULTS_FILE" ]; then
    RESULTS_FILE=$(mktemp "${TMPDIR:-/tmp}/homeboy-datamachine-agent.XXXXXX")
    PRINT_RESULTS=1
else
    PRINT_RESULTS=0
fi

CONFIG_JSON=$(jq -c . "$CONFIG_PATH")
COMPONENT_PATH=$(jq -r '.component_path // env.HOMEBOY_COMPONENT_PATH // empty' "$CONFIG_PATH")
COMPONENT_ID=$(jq -r '.component_id // env.HOMEBOY_COMPONENT_ID // empty' "$CONFIG_PATH")
if [ -z "$COMPONENT_PATH" ]; then
    COMPONENT_PATH="$(pwd)"
fi
if [ -z "$COMPONENT_ID" ]; then
    COMPONENT_ID="$(basename "$COMPONENT_PATH")"
fi

WORKLOAD_ID=$(jq -r '.workload_id // "datamachine-agent"' "$CONFIG_PATH")
WORKLOAD_LABEL=$(jq -r '.workload_label // "Run Data Machine agent"' "$CONFIG_PATH")
PLAYGROUND_WORDPRESS_VERSION=$(jq -r '.playground_wordpress_version // "7.0"' "$CONFIG_PATH")
BENCH_WARMUP_ITERATIONS=$(jq -r '.bench_warmup_iterations // 0' "$CONFIG_PATH")

SETTINGS_JSON=$(jq -nc \
    --arg workloadPath "$WORKLOAD_PLAYGROUND_PATH" \
    --arg workloadId "$WORKLOAD_ID" \
    --arg workloadLabel "$WORKLOAD_LABEL" \
    --arg wordpressVersion "$PLAYGROUND_WORDPRESS_VERSION" \
    --argjson config "$CONFIG_JSON" \
    --argjson warmup "$BENCH_WARMUP_ITERATIONS" \
    '{
        workload_run_before: ($config.workload_run_before // $config.bootstrap_run // []),
        workload_run_after: ($config.workload_run_after // []),
        validation_dependencies: ($config.validation_dependencies // $config.dependencies // []),
        playground_wordpress_version: $wordpressVersion,
        wp_config_defines: ($config.wp_config_defines // {}),
        playground_file_mounts: ($config.playground_file_mounts // []),
        playground_blueprint: ($config.playground_blueprint // {}),
        bench_site_mode: ($config.bench_site_mode // "fresh"),
        bench_warmup_iterations: $warmup,
        bench_env: (($config.bench_env // {}) + { HOMEBOY_DATAMACHINE_AGENT_CONFIG: ($config | tojson) }),
        playground_workloads: [
            {
                id: $workloadId,
                label: $workloadLabel,
                run: (($config.workload_run_before // $config.bootstrap_run // []) + [
                    { type: "php", file: $workloadPath }
                ] + ($config.workload_run_after // []))
            }
        ]
    }')

HOMEBOY_BENCH_RESULTS_FILE="$RESULTS_FILE" \
HOMEBOY_BENCH_ITERATIONS=1 \
HOMEBOY_BENCH_WARMUP_ITERATIONS="$BENCH_WARMUP_ITERATIONS" \
HOMEBOY_COMPONENT_ID="$COMPONENT_ID" \
HOMEBOY_COMPONENT_PATH="$COMPONENT_PATH" \
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_SETTINGS_JSON="$SETTINGS_JSON" \
    bash "$EXTENSION_PATH/scripts/bench/bench-runner.sh"

if [ "$PRINT_RESULTS" = "1" ]; then
    cat "$RESULTS_FILE"
fi

scenario='.scenarios[] | select(.id == "'"$WORKLOAD_ID"'")'
if ! jq -e "$scenario | .metrics.config_present_mean == 1" "$RESULTS_FILE" >/dev/null; then
    echo "ERROR: Data Machine agent workload did not run" >&2
    exit 1
fi

if jq -e "$scenario | .metadata.error?" "$RESULTS_FILE" >/dev/null; then
    echo "ERROR: Data Machine agent workload reported an error" >&2
    jq -r "$scenario | .metadata.error" "$RESULTS_FILE" >&2
    exit 1
fi
