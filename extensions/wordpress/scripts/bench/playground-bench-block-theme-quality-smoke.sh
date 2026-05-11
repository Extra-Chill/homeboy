#!/usr/bin/env bash
#
# Playground block theme quality probe smoke test (homeboy-extensions#577).
#
# Exercises the generic PHP-first quality helper through the configured
# Playground workload path that scenario graders use.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
FIXTURE_DIR="${EXTENSION_PATH}/tests/fixtures/playground-block-theme-quality"

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

RESULTS_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/bench-block-theme-quality-smoke.XXXXXX")
cleanup() {
    rm -f "$RESULTS_TMPFILE"
}
trap cleanup EXIT

SETTINGS_JSON=$(cat <<'JSON'
{
  "playground_workloads": [
    {
      "id": "block-theme-quality",
      "label": "Block theme quality probe",
      "run": [
        {
          "type": "php",
          "file": "workloads/collect-quality.php",
          "role": "grader"
        }
      ]
    }
  ],
  "bench_warmup_iterations": 0
}
JSON
)

echo "============================================"
echo "Playground block theme quality smoke test"
echo "============================================"
echo "Fixture:    $FIXTURE_DIR"
echo "Iterations: 1"
echo "Warmup:    0"
echo ""

HOMEBOY_SETTINGS_JSON="$SETTINGS_JSON" \
HOMEBOY_BENCH_RESULTS_FILE="$RESULTS_TMPFILE" \
HOMEBOY_BENCH_ITERATIONS=1 \
HOMEBOY_COMPONENT_ID=playground-block-theme-quality \
HOMEBOY_COMPONENT_PATH="$FIXTURE_DIR" \
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    bash "${SCRIPT_DIR}/bench-runner.sh"

if [ ! -s "$RESULTS_TMPFILE" ]; then
    echo "ERROR: results file empty or missing at $RESULTS_TMPFILE" >&2
    exit 1
fi

scenario='.scenarios[] | select(.id == "block-theme-quality")'
scenario_count=$(jq -r '.scenarios | length' "$RESULTS_TMPFILE")
if [ "$scenario_count" -ne 2 ]; then
    echo "ERROR: expected 2 scenarios (__bootstrap + quality probe), got $scenario_count" >&2
    exit 1
fi
echo "✓ scenarios length == 2 (__bootstrap + quality probe)"

for metric in \
    used_block_theme_mean \
    theme_json_present_mean \
    front_page_id_mean \
    pages_seen_mean \
    templates_seen_mean \
    template_parts_seen_mean \
    posts_with_blocks_mean \
    total_blocks_mean \
    core_html_blocks_mean \
    serialized_block_comments_mean \
    target_pages_seen_mean \
    target_total_blocks_mean \
    target_core_html_blocks_mean \
    raw_html_unconverted_mean \
    navigation_created_mean; do
    value=$(jq -r "$scenario | .metrics.${metric} // \"missing\"" "$RESULTS_TMPFILE")
    if [ "$value" = "missing" ]; then
        echo "ERROR: missing quality metric ${metric}" >&2
        exit 1
    fi
done
echo "✓ quality metrics emitted"

if [ "$(jq -r "$scenario | .metrics.target_pages_seen_mean" "$RESULTS_TMPFILE")" != "1" ]; then
    echo "ERROR: target front page was not counted" >&2
    exit 1
fi
if [ "$(jq -r "$scenario | .metrics.target_total_blocks_mean" "$RESULTS_TMPFILE")" != "3" ]; then
    echo "ERROR: target front page block count was not 3" >&2
    exit 1
fi
if [ "$(jq -r "$scenario | .metrics.target_core_html_blocks_mean" "$RESULTS_TMPFILE")" != "1" ]; then
    echo "ERROR: target core/html block count was not 1" >&2
    exit 1
fi
if [ "$(jq -r "$scenario | .metrics.raw_html_unconverted_mean" "$RESULTS_TMPFILE")" != "1" ]; then
    echo "ERROR: raw HTML without blocks was not detected" >&2
    exit 1
fi
if [ "$(jq -r "$scenario | .metrics.navigation_created_mean" "$RESULTS_TMPFILE")" != "1" ]; then
    echo "ERROR: navigation presence was not detected" >&2
    exit 1
fi
echo "✓ target, raw HTML, and navigation signals are correct"

if [ "$(jq -r "$scenario | .metadata.wordpress_quality.target_total_blocks" "$RESULTS_TMPFILE")" != "3" ]; then
    echo "ERROR: structured wordpress_quality metadata missing target_total_blocks" >&2
    exit 1
fi
echo "✓ structured metadata emitted"

echo ""
echo "============================================"
echo "✓ Block theme quality smoke test PASSED"
echo "============================================"
