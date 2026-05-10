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

REPO_ROOT="$(cd "$EXTENSION_PATH/.." && pwd)"
BUNDLE_REF="$(git -C "$REPO_ROOT" rev-parse HEAD)"

jq -n \
    --arg componentPath "$FIXTURE_DIR" \
    --arg bundleRepo "$REPO_ROOT" \
    --arg bundleRef "$BUNDLE_REF" \
    '{
        component_id: "playground-workloads",
        component_path: $componentPath,
        bundle_repo: $bundleRepo,
        bundle_ref: $bundleRef,
        bundle_path_in_repo: ".",
        workload_id: "datamachine-agent-dry-run",
        workload_label: "Data Machine agent dry run",
        dry_run: true,
        agent_slug: "example-agent",
        flow_slug: "example-flow",
        provider: "example-provider",
        model: "example-model",
        prompt: "Dry-run the generic Data Machine agent workload.",
        workload_run_before: [
            { type: "php", file: "workloads/datamachine-agent-bootstrap.php" }
        ]
    }' > "$CONFIG_TMPFILE"

HOMEBOY_DATAMACHINE_AGENT_RESULTS_FILE="$RESULTS_TMPFILE" \
    bash "$SCRIPT_DIR/run-datamachine-agent.sh" "$CONFIG_TMPFILE"

scenario='.scenarios[] | select(.id == "datamachine-agent-dry-run")'
if ! jq -e "$scenario" "$RESULTS_TMPFILE" >/dev/null; then
    echo "ERROR: datamachine-agent-dry-run scenario missing" >&2
    cat "$RESULTS_TMPFILE" >&2
    exit 1
fi

for metric in bootstrap_ran_mean config_present_mean dry_run_mean; do
    value=$(jq -r "$scenario | .metrics.${metric} // \"missing\"" "$RESULTS_TMPFILE")
    if [ "$value" != "1" ]; then
        echo "ERROR: ${metric} expected 1, got ${value}" >&2
        cat "$RESULTS_TMPFILE" >&2
        exit 1
    fi
done

provider=$(jq -r "$scenario | .metadata.provider // \"missing\"" "$RESULTS_TMPFILE")
model=$(jq -r "$scenario | .metadata.model // \"missing\"" "$RESULTS_TMPFILE")
bootstrap_value=$(jq -r "$scenario | .metadata.bootstrap_value // \"missing\"" "$RESULTS_TMPFILE")
bundle_repo=$(jq -r "$scenario | .metadata.bundle_repo // \"missing\"" "$RESULTS_TMPFILE")
bundle_ref=$(jq -r "$scenario | .metadata.bundle_ref // \"missing\"" "$RESULTS_TMPFILE")
bundle_path=$(jq -r "$scenario | .metadata.bundle_path // \"missing\"" "$RESULTS_TMPFILE")
if [ "$provider" != "example-provider" ] || [ "$model" != "example-model" ]; then
    echo "ERROR: provider/model metadata missing (provider=$provider model=$model)" >&2
    cat "$RESULTS_TMPFILE" >&2
    exit 1
fi
if [ "$bootstrap_value" != "ran" ]; then
    echo "ERROR: bootstrap metadata missing (bootstrap_value=$bootstrap_value)" >&2
    cat "$RESULTS_TMPFILE" >&2
    exit 1
fi
if [ "$bundle_repo" != "$REPO_ROOT" ] || [ "$bundle_ref" != "$BUNDLE_REF" ] || [ -z "$bundle_path" ] || [ "$bundle_path" = "missing" ]; then
    echo "ERROR: external bundle metadata missing (repo=$bundle_repo ref=$bundle_ref path=$bundle_path)" >&2
    cat "$RESULTS_TMPFILE" >&2
    exit 1
fi

echo "✓ Data Machine agent runner smoke test PASSED"
