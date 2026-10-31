#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-node-bench-workload-context.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

HELPER_JS="$TMP_DIR/bench-helper.mjs"
cat > "$HELPER_JS" <<'EOF'
import { basename } from 'node:path';
import { writeFile } from 'node:fs/promises';

export function homeboyBenchPercentile(values, percentile) {
  if (values.length === 0) return 0;
  if (values.length === 1) return values[0];
  const index = (values.length - 1) * percentile;
  const lower = Math.floor(index);
  const upper = Math.ceil(index);
  if (lower === upper) return values[lower];
  return values[lower] + (values[upper] - values[lower]) * (index - lower);
}

export function homeboyBenchScenarioId(file, suffixPattern) {
  return basename(file).replace(suffixPattern, '');
}

export async function homeboyWriteBenchResults(file, componentId, iterations, scenarios) {
  await writeFile(file, JSON.stringify({ component_id: componentId, iterations, scenarios }, null, 2));
}
EOF

PROJECT_DIR="$TMP_DIR/project"
mkdir -p "$PROJECT_DIR/bench"
printf '{"name":"node-bench-workload-context-smoke"}\n' > "$PROJECT_DIR/package.json"

cat > "$PROJECT_DIR/bench/context.bench.mjs" <<'EOF'
export default async function (context) {
  if (!context || typeof context !== 'object') throw new Error('missing workload context');
  if (context.args.join('\n') !== 'first\n--flag=value\nspaced arg') throw new Error(`unexpected context args: ${JSON.stringify(context.args)}`);
  if (!context.artifactsDir.endsWith('/artifacts')) throw new Error('missing artifactsDir');
  if (context.publicArtifactBaseUrl !== 'https://artifacts.example.test/run-1') throw new Error('missing publicArtifactBaseUrl');
  if (context.env.HOMEBOY_COMPONENT_ID !== 'node-bench-context-smoke') throw new Error('missing env passthrough');
  return { metrics: { arg_count: context.args.length } };
}
EOF

RESULTS_FILE="$TMP_DIR/bench-results.json"
HOMEBOY_RUNTIME_BENCH_HELPER_JS="$HELPER_JS" \
    HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
    HOMEBOY_COMPONENT_ID="node-bench-context-smoke" \
    HOMEBOY_BENCH_ITERATIONS="1" \
    HOMEBOY_BENCH_WARMUP_ITERATIONS="0" \
    HOMEBOY_BENCH_RESULTS_FILE="$RESULTS_FILE" \
    HOMEBOY_BENCH_ARTIFACTS_DIR="$TMP_DIR/artifacts" \
    HOMEBOY_BENCH_PUBLIC_ARTIFACT_BASE_URL="https://artifacts.example.test/run-1" \
    HOMEBOY_BENCH_ARGS_JSON='["first","--flag=value","spaced arg"]' \
    node "$SCRIPT_DIR/bench-runner.mjs" >/dev/null

node -e '
const fs = require("fs");
const data = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
if (data.scenarios[0].metrics.arg_count !== 3) throw new Error("workload did not receive context args");
' "$RESULTS_FILE"

set +e
INVALID_OUTPUT="$(HOMEBOY_RUNTIME_BENCH_HELPER_JS="$HELPER_JS" \
    HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
    HOMEBOY_COMPONENT_ID="node-bench-context-smoke" \
    HOMEBOY_BENCH_RESULTS_FILE="$TMP_DIR/invalid-results.json" \
    HOMEBOY_BENCH_ARGS_JSON='["ok",1]' \
    node "$SCRIPT_DIR/bench-runner.mjs" 2>&1)"
INVALID_EXIT=$?
set -e
if [ "$INVALID_EXIT" -eq 0 ]; then
    echo "expected invalid HOMEBOY_BENCH_ARGS_JSON to fail" >&2
    exit 1
fi
if [[ "$INVALID_OUTPUT" != *'HOMEBOY_BENCH_ARGS_JSON must be a JSON array of strings'* ]]; then
    echo "expected clear HOMEBOY_BENCH_ARGS_JSON validation error" >&2
    echo "$INVALID_OUTPUT" >&2
    exit 1
fi

echo "Node.js bench workload context smoke passed."
