#!/usr/bin/env bash
#
# Scenario manifest smoke test (homeboy-extensions#562).
#
# Points the Playground bench runner at one manifest file. The runner compiles
# that manifest into the existing playground_workloads path, then the smoke
# asserts prompt, grader, tags, limits, and metadata reach BenchResults.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
FIXTURE_DIR="${EXTENSION_PATH}/tests/fixtures/playground-scenario-manifest"

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

RESULTS_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/bench-scenario-manifest-smoke.XXXXXX")

cleanup() {
    rm -f "$RESULTS_TMPFILE"
}
trap cleanup EXIT

SETTINGS_JSON=$(cat <<'JSON'
{
  "playground_scenario_manifests": ["scenarios/manifest-001.json"],
  "bench_warmup_iterations": 0
}
JSON
)

echo "============================================"
echo "Playground bench scenario manifest smoke test"
echo "============================================"
echo "Fixture:    $FIXTURE_DIR"
echo "Iterations: 1"
echo "Warmup:    0"
echo ""

HOMEBOY_BENCH_RESULTS_FILE="$RESULTS_TMPFILE" \
HOMEBOY_BENCH_ITERATIONS=1 \
HOMEBOY_COMPONENT_ID=playground-scenario-manifest \
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

scenario='.scenarios[] | select(.id == "block-markup-navigation-001")'
if ! jq -e "$scenario" "$RESULTS_TMPFILE" >/dev/null; then
    echo "ERROR: scenario manifest workload missing" >&2
    exit 1
fi
echo "✓ scenario manifest compiled into a Playground workload"

grade=$(jq -r "$scenario | .metrics.grade_mean // \"missing\"" "$RESULTS_TMPFILE")
if [ "$grade" != "1" ]; then
    echo "ERROR: expected grade_mean=1, got $grade" >&2
    exit 1
fi
echo "✓ grader metric emitted"

prompt=$(jq -r "$scenario | .metadata.prompt // \"missing\"" "$RESULTS_TMPFILE")
prompt_file=$(jq -r "$scenario | .metadata.prompt_file // \"missing\"" "$RESULTS_TMPFILE")
grader_file=$(jq -r "$scenario | .metadata.grader_file // \"missing\"" "$RESULTS_TMPFILE")
max_turns=$(jq -r "$scenario | .metadata.limits.max_turns // \"missing\"" "$RESULTS_TMPFILE")
if [ "$prompt" != "Create a page containing valid navigation block markup." ] || [ "$prompt_file" != "scenarios/prompt.md" ] || [ "$grader_file" != "scenarios/grader.php" ] || [ "$max_turns" != "8" ]; then
    echo "ERROR: manifest metadata did not round-trip" >&2
    exit 1
fi
echo "✓ prompt file, grader file, and limits metadata emitted"

general_rule=$(jq -r "$scenario | .metadata.general_rules[0] // \"missing\"" "$RESULTS_TMPFILE")
task_rule=$(jq -r "$scenario | .metadata.task_rules[0] // \"missing\"" "$RESULTS_TMPFILE")
probe=$(jq -r "$scenario | .metadata.probes.behavioral_fingerprints[0].id // \"missing\"" "$RESULTS_TMPFILE")
if [ "$general_rule" != "wordpress_editable_blocks" ] || [ "$task_rule" != "navigation_block_markup" ] || [ "$probe" != "block_shape_fingerprint" ]; then
    echo "ERROR: manifest rule/probe metadata did not round-trip" >&2
    exit 1
fi
echo "✓ rule and probe metadata emitted"

tag=$(jq -r "$scenario | .tags[0] // \"missing\"" "$RESULTS_TMPFILE")
if [ "$tag" != "blocks" ]; then
    echo "ERROR: expected first tag to be blocks, got $tag" >&2
    exit 1
fi
echo "✓ tags emitted"

echo ""
echo "============================================"
echo "✓ Scenario manifest smoke test PASSED"
echo "============================================"
