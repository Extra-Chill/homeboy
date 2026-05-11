#!/usr/bin/env bash
#
# Playground grader schema smoke test (homeboy-extensions#563).
#
# Asserts configured Playground workloads can emit normalized grader output
# with binary success, partial reward, per-check details, and structured
# grader failure metadata when a grader step throws.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
FIXTURE_DIR="${EXTENSION_PATH}/tests/fixtures/playground-grader-schema"

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
    exit 1
fi

RESULTS_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/bench-grader-schema-smoke.XXXXXX")
cleanup() {
    rm -f "$RESULTS_TMPFILE"
}
trap cleanup EXIT

SETTINGS_JSON='{
  "playground_workloads": [
    {
      "id": "grader-success",
      "run": [
        { "type": "php", "file": "workloads/grader-success.php", "role": "grader" }
      ]
    },
    {
      "id": "grader-partial",
      "run": [
        { "type": "php", "file": "workloads/grader-partial.php", "role": "grader" }
      ]
    },
    {
      "id": "grader-throws",
      "run": [
        { "type": "php", "file": "workloads/grader-throws.php", "role": "grader", "id": "grader_runtime" }
      ]
    }
  ]
}'

echo "============================================"
echo "Playground bench grader schema smoke test"
echo "============================================"
echo "Fixture:    $FIXTURE_DIR"
echo "Iterations: 1 (per configured grader workload)"
echo ""

HOMEBOY_SETTINGS_JSON="$SETTINGS_JSON" \
HOMEBOY_BENCH_RESULTS_FILE="$RESULTS_TMPFILE" \
HOMEBOY_BENCH_ITERATIONS=1 \
HOMEBOY_COMPONENT_ID=playground-grader-schema \
HOMEBOY_COMPONENT_PATH="$FIXTURE_DIR" \
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    bash "${SCRIPT_DIR}/bench-runner.sh"

if [ ! -s "$RESULTS_TMPFILE" ]; then
    echo "ERROR: results file empty or missing at $RESULTS_TMPFILE" >&2
    exit 1
fi

scenario_count=$(jq -r '.scenarios | length' "$RESULTS_TMPFILE")
if [ "$scenario_count" -ne 4 ]; then
    echo "ERROR: expected 4 scenarios (__bootstrap + 3 graders), got $scenario_count" >&2
    exit 1
fi
echo "✓ scenarios length == 4 (__bootstrap + 3 grader workloads)"

success='.scenarios[] | select(.id == "grader-success")'
if [ "$(jq -r "$success | .metrics.success_mean" "$RESULTS_TMPFILE")" != "1" ]; then
    echo "ERROR: grader-success success_mean was not 1" >&2
    exit 1
fi
if [ "$(jq -r "$success | .metrics.reward_mean" "$RESULTS_TMPFILE")" != "1" ]; then
    echo "ERROR: grader-success reward_mean was not 1" >&2
    exit 1
fi
echo "✓ binary success and reward metrics emitted"

partial='.scenarios[] | select(.id == "grader-partial")'
if [ "$(jq -r "$partial | .metrics.reward_mean" "$RESULTS_TMPFILE")" != "0.5" ]; then
    echo "ERROR: grader-partial reward_mean was not 0.5" >&2
    exit 1
fi
if [ "$(jq -r "$partial | .metadata.grade.checks[1].passed" "$RESULTS_TMPFILE")" != "false" ]; then
    echo "ERROR: grader-partial failed check was not structured" >&2
    exit 1
fi
echo "✓ partial credit and failed check details emitted"

throws='.scenarios[] | select(.id == "grader-throws")'
if [ "$(jq -r "$throws | .metrics.reward_mean" "$RESULTS_TMPFILE")" != "0" ]; then
    echo "ERROR: grader-throws reward_mean was not 0" >&2
    exit 1
fi
if [ "$(jq -r "$throws | .metadata.grade.failure.type" "$RESULTS_TMPFILE")" != "RuntimeException" ]; then
    echo "ERROR: grader-throws failure type was not structured" >&2
    exit 1
fi
if [ "$(jq -r "$throws | .metadata.grade.checks[0].id" "$RESULTS_TMPFILE")" != "grader_runtime" ]; then
    echo "ERROR: grader-throws failure check id was not preserved" >&2
    exit 1
fi
echo "✓ thrown grader failure became structured grade metadata"

echo ""
echo "============================================"
echo "✓ Grader schema smoke test PASSED"
echo "============================================"
