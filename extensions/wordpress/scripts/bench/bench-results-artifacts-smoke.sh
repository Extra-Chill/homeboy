#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=bench-results-artifacts.sh
source "${SCRIPT_DIR}/bench-results-artifacts.sh"
# shellcheck source=bench-artifact-viewer-contract.sh
source "${SCRIPT_DIR}/bench-artifact-viewer-contract.sh"

if ! command -v jq >/dev/null 2>&1; then
    echo "ERROR: jq required for JSON assertions in this smoke." >&2
    exit 1
fi

TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/bench-results-artifacts.XXXXXX")
PUBLIC_SERVER_PID=""
cleanup() {
    if [ -n "$PUBLIC_SERVER_PID" ]; then
        kill "$PUBLIC_SERVER_PID" >/dev/null 2>&1 || true
    fi
    rm -rf "$TMP_ROOT"
}
trap cleanup EXIT

RESULTS_FILE="${TMP_ROOT}/bench-results.json"
BASELINE_RESULTS_FILE="${TMP_ROOT}/bench-baseline-results.json"

cat > "$RESULTS_FILE" <<'JSON'
{
  "component_id": "wp-rl-fixture",
  "iterations": 1,
  "scenarios": [
    {
      "id": "__bootstrap",
      "iterations": 1,
      "metrics": { "boot_ms": 12 }
    },
    {
      "id": "block-markup/navigation-001",
      "iterations": 1,
      "metrics": { "mean_ms": 1234, "reward_mean": 1, "success_mean": 1, "turns_mean": 7 },
      "metadata": {
        "provider": "openai",
        "model": "gpt-5.5",
        "seed": 1,
        "tokens": { "input": 1000, "output": 500 },
        "step_series": [
          {
            "type": "request",
            "label": "GET /shop/",
            "url": "/shop/",
            "status": "pass",
            "status_code": 200,
            "elapsed_ms": 25.5,
            "metrics": { "db_queries": 12 },
            "metadata": { "cache_state": "cold" }
          },
          {
            "type": "option_sample",
            "label": "Transient count",
            "option": "_transient_wc_layered_nav_counts",
            "status": "fail",
            "failure": { "message": "transient grew past budget" },
            "metadata": { "transient_count": 42, "budget": 30 }
          }
        ]
      },
      "artifacts": {
        "transcript": { "path": "artifacts/transcript.json", "kind": "json" },
        "blueprint.after": {
          "path": "artifacts/blueprint.after.json",
          "kind": "json",
          "label": "Generated site replay",
          "viewer": {
            "kind": "wordpress-playground-blueprint",
            "base": "https://playground.wordpress.net/",
            "query": {
              "parameter": "blueprint-url",
              "value": { "source": "public-artifact-url" },
              "encoding": "url"
            }
          }
        },
        "runtime_url": { "path": "https://example.test", "kind": "url" }
      }
    },
    {
      "id": "block-markup/query-002",
      "iterations": 1,
      "metrics": { "mean_ms": 500, "reward_mean": 0 },
      "metadata": { "provider": "openai", "model": "gpt-5.5", "seed": 2 },
      "artifacts": {
        "step_series": {
          "path": "artifacts/query-series.json",
          "kind": "json",
          "schema": "homeboy/wordpress-bench-step-series/v1"
        }
      },
      "error": { "message": "scenario failed" }
    }
  ]
}
JSON

cat > "$BASELINE_RESULTS_FILE" <<'JSON'
{
  "component_id": "wp-rl-fixture",
  "iterations": 1,
  "scenarios": [
    {
      "id": "block-markup/navigation-001",
      "iterations": 1,
      "metrics": { "mean_ms": 1400, "reward_mean": 1, "success_mean": 1, "turns_mean": 8 }
    },
    {
      "id": "block-markup/query-002",
      "iterations": 1,
      "metrics": { "mean_ms": 500, "reward_mean": 0 }
    }
  ]
}
JSON

HOMEBOY_BENCH_BASELINE_RESULTS_FILE="$BASELINE_RESULTS_FILE" \
HOMEBOY_BENCH_WEBPERF_SUMMARY_METRICS_JSON='["mean_ms","reward_mean"]' \
homeboy_wordpress_emit_bench_results_artifacts "$RESULTS_FILE"

JSONL_FILE="${TMP_ROOT}/results.jsonl"
LEADERBOARD_FILE="${TMP_ROOT}/leaderboard.md"
SERIES_FILE="${TMP_ROOT}/series.json"
WEBPERF_SUMMARY_JSON_FILE="${TMP_ROOT}/webperf-evidence-summary.json"
WEBPERF_SUMMARY_MARKDOWN_FILE="${TMP_ROOT}/webperf-evidence-summary.md"
PUBLIC_ROOT="${TMP_ROOT}/public"
PUBLIC_PORT_FILE="${TMP_ROOT}/public-port"

if [ ! -s "$JSONL_FILE" ]; then
    echo "ERROR: missing results.jsonl artifact" >&2
    exit 1
fi
if [ ! -s "$LEADERBOARD_FILE" ]; then
    echo "ERROR: missing leaderboard.md artifact" >&2
    exit 1
fi
if [ ! -s "$SERIES_FILE" ]; then
    echo "ERROR: missing series.json artifact" >&2
    exit 1
fi
if [ ! -s "$WEBPERF_SUMMARY_JSON_FILE" ] || [ ! -s "$WEBPERF_SUMMARY_MARKDOWN_FILE" ]; then
    echo "ERROR: missing webperf evidence summary artifacts" >&2
    exit 1
fi

row_count=$(wc -l < "$JSONL_FILE" | tr -d ' ')
if [ "$row_count" -ne 2 ]; then
    echo "ERROR: expected 2 JSONL rows, got $row_count" >&2
    cat "$JSONL_FILE" >&2
    exit 1
fi

scenario='. | select(.scenario_id == "block-markup/navigation-001")'
provider=$(jq -r "$scenario | .provider" "$JSONL_FILE")
model=$(jq -r "$scenario | .model" "$JSONL_FILE")
success=$(jq -r "$scenario | .success" "$JSONL_FILE")
reward=$(jq -r "$scenario | .reward" "$JSONL_FILE")
duration=$(jq -r "$scenario | .duration_ms" "$JSONL_FILE")
artifact=$(jq -r "$scenario | .artifacts.transcript.path" "$JSONL_FILE")
viewer_source=$(jq -r "$scenario | .artifacts[\"blueprint.after\"].viewer.query.value.source" "$JSONL_FILE")

if [ "$provider" != "openai" ] || [ "$model" != "gpt-5.5" ] || [ "$success" != "true" ] || [ "$reward" != "1" ] || [ "$duration" != "1234" ] || [ "$artifact" != "artifacts/transcript.json" ]; then
    echo "ERROR: JSONL row missing expected scenario fields" >&2
    cat "$JSONL_FILE" >&2
    exit 1
fi
if [ "$viewer_source" != "public-artifact-url" ]; then
    echo "ERROR: JSONL row did not preserve generated-site viewer metadata" >&2
    cat "$JSONL_FILE" >&2
    exit 1
fi

error_count=$(grep -c 'scenario failed' "$JSONL_FILE" || true)
if [ "$error_count" -ne 1 ]; then
    echo "ERROR: JSONL did not preserve error row" >&2
    cat "$JSONL_FILE" >&2
    exit 1
fi

if ! grep -q '| openai | gpt-5.5 | 2 | 50% | 1 | 0.5 | 867 |' "$LEADERBOARD_FILE"; then
    echo "ERROR: leaderboard did not aggregate success/error rows as expected" >&2
    cat "$LEADERBOARD_FILE" >&2
    exit 1
fi

series_schema=$(jq -r '.schema' "$SERIES_FILE")
series_count=$(jq -r '.series | length' "$SERIES_FILE")
request_success=$(jq -r '.series[] | select(.scenario_id == "block-markup/navigation-001") | .rows[] | select(.type == "request") | .success' "$SERIES_FILE")
option_failure=$(jq -r '.series[] | select(.scenario_id == "block-markup/navigation-001") | .rows[] | select(.type == "option_sample") | .failure.message' "$SERIES_FILE")
artifact_path=$(jq -r '.series[] | select(.scenario_id == "block-markup/query-002") | .artifact.path' "$SERIES_FILE")

if [ "$series_schema" != "homeboy/wordpress-bench-step-series/v1" ] || [ "$series_count" -ne 2 ] || [ "$request_success" != "true" ] || [ "$option_failure" != "transient grew past budget" ] || [ "$artifact_path" != "artifacts/query-series.json" ]; then
    echo "ERROR: series.json did not preserve normalized step-series rows and artifact refs" >&2
    cat "$SERIES_FILE" >&2
    exit 1
fi

webperf_schema=$(jq -r '.schema' "$WEBPERF_SUMMARY_JSON_FILE")
webperf_verdict=$(jq -r '.verdict' "$WEBPERF_SUMMARY_JSON_FILE")
webperf_row_count=$(jq -r '.measurement_rows | length' "$WEBPERF_SUMMARY_JSON_FILE")
webperf_mean_result=$(jq -r '.measurement_rows[] | select(.scenario_id == "block-markup/navigation-001" and .metric == "mean_ms") | .verdict' "$WEBPERF_SUMMARY_JSON_FILE")

if [ "$webperf_schema" != "homeboy/webperf-evidence-summary/v1" ] || [ "$webperf_verdict" != "improvement" ] || [ "$webperf_row_count" -ne 4 ] || [ "$webperf_mean_result" != "improvement" ]; then
    echo "ERROR: webperf summary did not capture focused baseline/candidate measurements" >&2
    cat "$WEBPERF_SUMMARY_JSON_FILE" >&2
    exit 1
fi
if ! grep -q 'Web Performance Evidence Summary' "$WEBPERF_SUMMARY_MARKDOWN_FILE"; then
    echo "ERROR: webperf markdown summary missing title" >&2
    cat "$WEBPERF_SUMMARY_MARKDOWN_FILE" >&2
    exit 1
fi

mkdir -p "${PUBLIC_ROOT}/published/artifacts"
printf '{"landingPage":"/"}\n' > "${PUBLIC_ROOT}/published/artifacts/blueprint.after.json"

PUBLIC_ROOT="$PUBLIC_ROOT" PUBLIC_PORT_FILE="$PUBLIC_PORT_FILE" node -e '
const fs = require("fs");
const http = require("http");
const path = require("path");

const root = path.resolve(process.env.PUBLIC_ROOT);
const portFile = process.env.PUBLIC_PORT_FILE;
const server = http.createServer((request, response) => {
  const requestUrl = new URL(request.url, "http://127.0.0.1");
  const requestedPath = path.resolve(root, `.${decodeURIComponent(requestUrl.pathname)}`);
  if (requestedPath !== root && !requestedPath.startsWith(`${root}${path.sep}`)) {
    response.writeHead(403);
    response.end("forbidden");
    return;
  }

  fs.readFile(requestedPath, (error, data) => {
    if (error) {
      response.writeHead(404);
      response.end("not found");
      return;
    }

    response.writeHead(200, { "content-type": "application/json" });
    response.end(request.method === "HEAD" ? undefined : data);
  });
});

server.listen(0, "127.0.0.1", () => {
  fs.writeFileSync(portFile, String(server.address().port));
});
' &
PUBLIC_SERVER_PID="$!"

for _ in 1 2 3 4 5 6 7 8 9 10; do
    if [ -s "$PUBLIC_PORT_FILE" ]; then
        break
    fi
    sleep 0.1
done

if [ ! -s "$PUBLIC_PORT_FILE" ]; then
    echo "ERROR: public artifact fixture server did not start" >&2
    exit 1
fi

PUBLIC_BASE_URL="http://127.0.0.1:$(cat "$PUBLIC_PORT_FILE")/published/"
PUBLIC_ARTIFACT_URL="$(homeboy_bench_artifact_public_url "artifacts/blueprint.after.json" "$PUBLIC_BASE_URL")"
EXPECTED_PUBLIC_ARTIFACT_URL="${PUBLIC_BASE_URL}artifacts/blueprint.after.json"

if [ "$PUBLIC_ARTIFACT_URL" != "$EXPECTED_PUBLIC_ARTIFACT_URL" ]; then
    echo "ERROR: public artifact URL helper resolved unexpected URL" >&2
    echo "expected: $EXPECTED_PUBLIC_ARTIFACT_URL" >&2
    echo "actual:   $PUBLIC_ARTIFACT_URL" >&2
    exit 1
fi

if ! homeboy_bench_require_public_artifact_reachable "$PUBLIC_ARTIFACT_URL"; then
    echo "ERROR: public artifact URL was not reachable: $PUBLIC_ARTIFACT_URL" >&2
    exit 1
fi

VIEWER_JSON="$(homeboy_bench_playground_blueprint_viewer_json "artifacts/blueprint.after.json" "$PUBLIC_BASE_URL")"
VIEWER_URL="$(printf '%s\n' "$VIEWER_JSON" | jq -r '.url')"
VIEWER_KIND="$(printf '%s\n' "$VIEWER_JSON" | jq -r '.kind')"
VIEWER_SOURCE="$(printf '%s\n' "$VIEWER_JSON" | jq -r '.query.value.source')"
VIEWER_PUBLIC_ARTIFACT_URL="$(printf '%s\n' "$VIEWER_JSON" | jq -r '.query.value.url')"
VIEWER_BLUEPRINT_URL="$(node -e '
const viewerUrl = new URL(process.argv[1]);
console.log(viewerUrl.searchParams.get("blueprint-url"));
' "$VIEWER_URL")"

if [ "$VIEWER_KIND" != "wordpress-playground-blueprint" ] || [ "$VIEWER_SOURCE" != "public-artifact-url" ] || [ "$VIEWER_PUBLIC_ARTIFACT_URL" != "$PUBLIC_ARTIFACT_URL" ] || [ "$VIEWER_BLUEPRINT_URL" != "$PUBLIC_ARTIFACT_URL" ]; then
    echo "ERROR: resolved viewer metadata did not point at the reachable public artifact" >&2
    printf '%s\n' "$VIEWER_JSON" >&2
    exit 1
fi

echo "✓ Bench results artifacts smoke test PASSED"
