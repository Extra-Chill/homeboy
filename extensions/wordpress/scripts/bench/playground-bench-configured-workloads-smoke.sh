#!/usr/bin/env bash
#
# Config-declared Playground workloads smoke test (homeboy-extensions#420).
#
# Runs a fixture with no tests/bench directory and supplies a
# playground_workloads setting. The configured PHP step returns the same
# { metrics, artifacts, metadata } shape as Node.js workloads and the smoke
# asserts those values reach the BenchResults scenario envelope.

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
    echo "ERROR: jq required for JSON assertions in this smoke." >&2
    echo "Install: brew install jq (macOS) or your package manager." >&2
    exit 1
fi

RESULTS_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/bench-configured-workloads-smoke.XXXXXX")

cleanup() {
    rm -f "$RESULTS_TMPFILE" "${FIXTURE_DIR}/workloads/report.json" "${FIXTURE_DIR}/workloads/count.txt"
}
trap cleanup EXIT

SETTINGS_JSON=$(cat <<'JSON'
{
  "playground_workloads": [
    {
      "id": "generated-site-preview",
      "label": "Generated site preview",
      "run": [
        {
          "type": "php",
          "file": "workloads/configured.php"
        }
      ],
      "artifacts": {
        "configured_report": {
          "path": "workloads/report.json",
          "kind": "json",
          "label": "Configured workload report"
        }
      },
      "metadata": {
        "source": "settings"
      }
    }
  ],
  "bench_warmup_iterations": 0
}
JSON
)

echo "============================================"
echo "Playground bench configured workloads smoke test"
echo "============================================"
echo "Fixture:    $FIXTURE_DIR"
echo "Iterations: 2 (per configured workload)"
echo "Warmup:    0"
echo ""

HOMEBOY_BENCH_RESULTS_FILE="$RESULTS_TMPFILE" \
HOMEBOY_BENCH_ITERATIONS=2 \
HOMEBOY_COMPONENT_ID=playground-workloads \
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
    echo "ERROR: expected 2 scenarios (__bootstrap + configured workload), got $scenario_count" >&2
    exit 1
fi
echo "✓ scenarios length == 2 (__bootstrap + configured workload)"

scenario='.scenarios[] | select(.id == "generated-site-preview")'

source=$(jq -r "$scenario | .source" "$RESULTS_TMPFILE")
if [ "$source" != "config" ]; then
    echo "ERROR: expected source=config, got $source" >&2
    exit 1
fi
echo "✓ configured workload source emitted"

for metric in generated_pages_mean generated_pages_p50 generated_pages_p95 generated_pages_p99 generated_pages_min generated_pages_max; do
    value=$(jq -r "$scenario | .metrics.${metric} // \"missing\"" "$RESULTS_TMPFILE")
    if [ "$value" != "2" ]; then
        echo "ERROR: ${metric} expected 2, got ${value}" >&2
        exit 1
    fi
done
echo "✓ configured workload metrics aggregated"

phase=$(jq -r "$scenario | .metadata.phase // \"missing\"" "$RESULTS_TMPFILE")
preview_url=$(jq -r "$scenario | .metadata.preview_url // \"missing\"" "$RESULTS_TMPFILE")
if [ "$phase" != "configured" ] || [ "$preview_url" != "https://example.test/playground-preview" ]; then
    echo "ERROR: metadata missing expected phase/preview_url" >&2
    exit 1
fi
echo "✓ configured workload metadata emitted"

artifact_path=$(jq -r "$scenario | .artifacts.generated_report.path // \"missing\"" "$RESULTS_TMPFILE")
artifact_label=$(jq -r "$scenario | .artifacts.generated_report.label // \"missing\"" "$RESULTS_TMPFILE")
if [ "$artifact_path" != "workloads/report.json" ] || [ "$artifact_label" != "Generated workload report" ]; then
    echo "ERROR: generated_report artifact missing expected path/label" >&2
    exit 1
fi
echo "✓ configured workload artifacts emitted"

run_count=$(jq -r '.run_count // "missing"' "${FIXTURE_DIR}/workloads/report.json")
if [ "$run_count" != "2" ]; then
    echo "ERROR: expected configured workload to run exactly 2 times with zero warmup, got $run_count" >&2
    exit 1
fi
echo "✓ configured workload warmup can be disabled"

echo ""
echo "============================================"
echo "✓ Configured workloads smoke test PASSED"
echo "============================================"
