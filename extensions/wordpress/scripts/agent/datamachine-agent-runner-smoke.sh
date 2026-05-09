#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
FIXTURE_DIR="${EXTENSION_PATH}/tests/fixtures/playground-workloads"

if [ ! -d "$FIXTURE_DIR" ]; then
    echo "ERROR: fixture not found at $FIXTURE_DIR" >&2
    exit 1
fi
if [ ! -f "${EXTENSION_PATH}/node_modules/.bin/wp-playground-cli" ]; then
    echo "ERROR: @wp-playground/cli not installed." >&2
    echo "Run: cd ${EXTENSION_PATH} && npm install" >&2
    exit 1
fi
if [ ! -d "${EXTENSION_PATH}/vendor/wp-phpunit" ]; then
    echo "ERROR: wp-phpunit not installed." >&2
    echo "Run: cd ${EXTENSION_PATH} && composer install" >&2
    exit 1
fi
if ! command -v jq >/dev/null 2>&1; then
    echo "ERROR: jq required" >&2
    exit 1
fi

CONFIG_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/datamachine-agent-config.XXXXXX.json")
RESULTS_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/datamachine-agent-results.XXXXXX.json")
cleanup() {
    rm -f "$CONFIG_TMPFILE" "$RESULTS_TMPFILE"
}
trap cleanup EXIT

jq -n \
    --arg componentPath "$FIXTURE_DIR" \
    '{
        component_id: "playground-workloads",
        component_path: $componentPath,
        workload_id: "datamachine-agent-dry-run",
        workload_label: "Data Machine agent dry run",
        dry_run: true,
        agent_slug: "example-agent",
        flow_slug: "example-flow",
        provider: "example-provider",
        model: "example-model",
        prompt: "Dry-run the generic Data Machine agent workload."
    }' > "$CONFIG_TMPFILE"

HOMEBOY_DATAMACHINE_AGENT_RESULTS_FILE="$RESULTS_TMPFILE" \
    bash "$SCRIPT_DIR/run-datamachine-agent.sh" "$CONFIG_TMPFILE"

scenario='.scenarios[] | select(.id == "datamachine-agent-dry-run")'
if ! jq -e "$scenario" "$RESULTS_TMPFILE" >/dev/null; then
    echo "ERROR: datamachine-agent-dry-run scenario missing" >&2
    cat "$RESULTS_TMPFILE" >&2
    exit 1
fi

for metric in config_present_mean dry_run_mean; do
    value=$(jq -r "$scenario | .metrics.${metric} // \"missing\"" "$RESULTS_TMPFILE")
    if [ "$value" != "1" ]; then
        echo "ERROR: ${metric} expected 1, got ${value}" >&2
        cat "$RESULTS_TMPFILE" >&2
        exit 1
    fi
done

provider=$(jq -r "$scenario | .metadata.provider // \"missing\"" "$RESULTS_TMPFILE")
model=$(jq -r "$scenario | .metadata.model // \"missing\"" "$RESULTS_TMPFILE")
if [ "$provider" != "example-provider" ] || [ "$model" != "example-model" ]; then
    echo "ERROR: provider/model metadata missing (provider=$provider model=$model)" >&2
    cat "$RESULTS_TMPFILE" >&2
    exit 1
fi

echo "✓ Data Machine agent runner smoke test PASSED"
