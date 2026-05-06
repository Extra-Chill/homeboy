#!/usr/bin/env bash
#
# Configured Playground workload `ability` step smoke (homeboy-extensions#420).
#
# Boots WordPress Playground, loads a fixture plugin that registers an
# Abilities-API-shaped ability, and asserts a configured workload can invoke
# the ability via the new step type and surface metrics/artifacts/metadata in
# the BenchResults envelope.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
FIXTURE_DIR="${EXTENSION_PATH}/tests/fixtures/playground-workloads-ability"

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
    echo "ERROR: jq required for JSON assertions in this smoke." >&2
    echo "Install: brew install jq (macOS) or your package manager." >&2
    exit 1
fi

RESULTS_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/bench-ability-step-smoke.XXXXXX")
cleanup() {
    rm -f "$RESULTS_TMPFILE"
}
trap cleanup EXIT

SETTINGS_JSON=$(cat <<'JSON'
{
  "playground_workloads": [
    {
      "id": "ability-pipeline",
      "label": "Ability-driven pipeline",
      "run": [
        {
          "type": "ability",
          "ability": "playground-workloads-fixture/run-pipeline",
          "input": { "pipeline_id": 42, "items": 7 }
        }
      ]
    }
  ]
}
JSON
)

echo "============================================"
echo "Playground bench ability step smoke test"
echo "============================================"
echo "Fixture:    $FIXTURE_DIR"
echo "Iterations: 2 (per configured workload)"
echo ""

HOMEBOY_BENCH_RESULTS_FILE="$RESULTS_TMPFILE" \
HOMEBOY_BENCH_ITERATIONS=2 \
HOMEBOY_COMPONENT_ID=playground-workloads-ability \
HOMEBOY_COMPONENT_PATH="$FIXTURE_DIR" \
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_SETTINGS_JSON="$SETTINGS_JSON" \
    bash "${SCRIPT_DIR}/bench-runner.sh"

if [ ! -s "$RESULTS_TMPFILE" ]; then
    echo "ERROR: results file empty or missing at $RESULTS_TMPFILE" >&2
    exit 1
fi

echo ""
echo "--- Results envelope ---"
cat "$RESULTS_TMPFILE"
echo ""

scenario_count=$(jq -r '.scenarios | length' "$RESULTS_TMPFILE")
if [ "$scenario_count" -ne 2 ]; then
    echo "ERROR: expected 2 scenarios (__bootstrap + ability workload), got $scenario_count" >&2
    exit 1
fi
echo "✓ scenarios length == 2 (__bootstrap + ability workload)"

scenario='.scenarios[] | select(.id == "ability-pipeline")'

source=$(jq -r "$scenario | .source" "$RESULTS_TMPFILE")
if [ "$source" != "config" ]; then
    echo "ERROR: expected source=config, got $source" >&2
    exit 1
fi
echo "✓ ability workload source emitted"

for metric in items_processed_mean items_processed_p50 items_processed_p95 items_processed_p99 items_processed_min items_processed_max; do
    value=$(jq -r "$scenario | .metrics.${metric} // \"missing\"" "$RESULTS_TMPFILE")
    if [ "$value" != "7" ]; then
        echo "ERROR: ${metric} expected 7, got ${value}" >&2
        exit 1
    fi
done
echo "✓ ability metrics aggregated"

phase=$(jq -r "$scenario | .metadata.phase // \"missing\"" "$RESULTS_TMPFILE")
pipeline_id=$(jq -r "$scenario | .metadata.pipeline_id // \"missing\"" "$RESULTS_TMPFILE")
if [ "$phase" != "ability-executed" ] || [ "$pipeline_id" != "42" ]; then
    echo "ERROR: ability metadata missing expected phase/pipeline_id (phase=$phase pipeline_id=$pipeline_id)" >&2
    exit 1
fi
echo "✓ ability metadata emitted"

artifact_path=$(jq -r "$scenario | .artifacts.ability_report.path // \"missing\"" "$RESULTS_TMPFILE")
if [ "$artifact_path" != "wp-content/playground-workloads-fixture/ability-report.json" ]; then
    echo "ERROR: ability_report artifact missing expected path (got $artifact_path)" >&2
    exit 1
fi
echo "✓ ability artifact emitted"

echo ""
echo "============================================"
echo "✓ Ability step smoke test PASSED"
echo "============================================"
