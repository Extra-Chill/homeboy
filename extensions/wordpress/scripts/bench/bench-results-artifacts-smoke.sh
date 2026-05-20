#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=bench-results-artifacts.sh
source "${SCRIPT_DIR}/bench-results-artifacts.sh"

if ! command -v jq >/dev/null 2>&1; then
    echo "ERROR: jq required for JSON assertions in this smoke." >&2
    exit 1
fi

TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/bench-results-artifacts.XXXXXX")
cleanup() {
    rm -rf "$TMP_ROOT"
}
trap cleanup EXIT

RESULTS_FILE="${TMP_ROOT}/bench-results.json"

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
        "tokens": { "input": 1000, "output": 500 }
      },
      "artifacts": {
        "transcript": { "path": "artifacts/transcript.json", "kind": "json" },
        "runtime_url": { "path": "https://example.test", "kind": "url" }
      }
    },
    {
      "id": "block-markup/query-002",
      "iterations": 1,
      "metrics": { "mean_ms": 500, "reward_mean": 0 },
      "metadata": { "provider": "openai", "model": "gpt-5.5", "seed": 2 },
      "error": { "message": "scenario failed" }
    }
  ]
}
JSON

homeboy_wordpress_emit_bench_results_artifacts "$RESULTS_FILE"

JSONL_FILE="${TMP_ROOT}/results.jsonl"
LEADERBOARD_FILE="${TMP_ROOT}/leaderboard.md"

if [ ! -s "$JSONL_FILE" ]; then
    echo "ERROR: missing results.jsonl artifact" >&2
    exit 1
fi
if [ ! -s "$LEADERBOARD_FILE" ]; then
    echo "ERROR: missing leaderboard.md artifact" >&2
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

if [ "$provider" != "openai" ] || [ "$model" != "gpt-5.5" ] || [ "$success" != "true" ] || [ "$reward" != "1" ] || [ "$duration" != "1234" ] || [ "$artifact" != "artifacts/transcript.json" ]; then
    echo "ERROR: JSONL row missing expected scenario fields" >&2
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

echo "✓ Bench results artifacts smoke test PASSED"
