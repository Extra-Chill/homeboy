#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-node-bench-artifacts.XXXXXX")"
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

assert_json() {
    local file="$1"
    local script="$2"
    shift 2
    node -e "const fs = require('fs'); const data = JSON.parse(fs.readFileSync(process.argv[1], 'utf8')); ${script}" "$file" "$@"
}

make_project() {
    local name="$1"
    local project_dir="$TMP_DIR/$name"
    mkdir -p "$project_dir/bench"
    printf '{"name":"%s"}\n' "$name" > "$project_dir/package.json"
    printf '%s\n' "$project_dir"
}

run_runner() {
    local project_dir="$1"
    local results_file="$2"
    local artifacts_dir="${3:-}"
    HOMEBOY_RUNTIME_BENCH_HELPER_JS="$HELPER_JS" \
    HOMEBOY_COMPONENT_PATH="$project_dir" \
    HOMEBOY_COMPONENT_ID="node-bench-artifacts-smoke" \
    HOMEBOY_BENCH_ITERATIONS="1" \
    HOMEBOY_BENCH_WARMUP_ITERATIONS="0" \
    HOMEBOY_BENCH_ARTIFACTS_DIR="$artifacts_dir" \
    HOMEBOY_BENCH_PUBLIC_ARTIFACT_BASE_URL="${HOMEBOY_BENCH_PUBLIC_ARTIFACT_BASE_URL:-}" \
    HOMEBOY_BENCH_RESULTS_FILE="$results_file" \
        node "$SCRIPT_DIR/bench-runner.mjs"
}

METRICS_ONLY_PROJECT="$(make_project metrics-only)"
cat > "$METRICS_ONLY_PROJECT/bench/metrics.bench.mjs" <<'EOF'
export default async function () {
  return { metrics: { success_rate: 1 } };
}
EOF

METRICS_ONLY_RESULTS="$TMP_DIR/metrics-only-results.json"
run_runner "$METRICS_ONLY_PROJECT" "$METRICS_ONLY_RESULTS" >/dev/null
assert_json "$METRICS_ONLY_RESULTS" '
const scenario = data.scenarios[0];
if (scenario.metrics.success_rate !== 1) throw new Error("metrics-only workload lost custom metric");
if (Object.prototype.hasOwnProperty.call(scenario, "artifacts")) throw new Error("metrics-only workload should not emit artifacts");
'

ARTIFACT_PROJECT="$(make_project artifacts)"
ARTIFACTS_DIR="$TMP_DIR/published-artifacts"
mkdir -p "$ARTIFACTS_DIR"
cat > "$ARTIFACT_PROJECT/bench/artifacts.bench.mjs" <<'EOF'
export default async function () {
  const artifactsDir = process.env.HOMEBOY_BENCH_ARTIFACTS_DIR;
  return {
    metrics: { success_rate: 1 },
    artifacts: {
      raw_result: {
        path: `${artifactsDir}/result.json`,
        kind: 'json',
        label: 'Raw result',
        viewer: {
          kind: 'opaque-review-viewer',
          url: 'https://viewer.example.test/runs/fixture',
          nested: { preserved: true },
        },
      },
      site_path: { path: '/tmp/site', kind: 'directory', label: 'Generated site' },
      bare_path: '/tmp/bare.txt',
      relative_evidence: { path: 'evidence.json', kind: 'json', label: 'Evidence' },
    },
  };
}
EOF

ARTIFACT_RESULTS="$TMP_DIR/artifact-results.json"
HOMEBOY_BENCH_PUBLIC_ARTIFACT_BASE_URL="https://artifacts.example.test/runs/run-123" \
run_runner "$ARTIFACT_PROJECT" "$ARTIFACT_RESULTS" "$ARTIFACTS_DIR" >/dev/null
assert_json "$ARTIFACT_RESULTS" '
const artifacts = data.scenarios[0].artifacts;
if (!artifacts) throw new Error("scenario artifacts missing");
if (artifacts.raw_result.path !== process.argv[2] + "/result.json") throw new Error("raw_result path missing");
if (artifacts.raw_result.kind !== "json") throw new Error("raw_result kind missing");
if (artifacts.raw_result.label !== "Raw result") throw new Error("raw_result label missing");
if (artifacts.raw_result.url !== "https://artifacts.example.test/runs/run-123/result.json") throw new Error("raw_result public URL missing");
if (artifacts.raw_result.viewer.kind !== "opaque-review-viewer") throw new Error("viewer kind missing");
if (artifacts.raw_result.viewer.url !== "https://viewer.example.test/runs/fixture") throw new Error("viewer URL missing");
if (artifacts.raw_result.viewer.nested.preserved !== true) throw new Error("viewer metadata was not preserved");
if (artifacts.site_path.kind !== "directory") throw new Error("site_path kind missing");
if (artifacts.bare_path.path !== "/tmp/bare.txt") throw new Error("bare path was not normalized");
if (artifacts.relative_evidence.url !== "https://artifacts.example.test/runs/run-123/evidence.json") throw new Error("relative evidence public URL missing");
' "$ARTIFACTS_DIR"

INVALID_PROJECT="$(make_project invalid-artifact)"
cat > "$INVALID_PROJECT/bench/invalid.bench.mjs" <<'EOF'
export default async function () {
  return { artifacts: { raw_result: { kind: 'json' } } };
}
EOF

set +e
INVALID_OUTPUT="$(run_runner "$INVALID_PROJECT" "$TMP_DIR/invalid-results.json" 2>&1)"
INVALID_EXIT=$?
set -e
if [ "$INVALID_EXIT" -eq 0 ]; then
    echo "expected invalid artifact workload to fail" >&2
    exit 1
fi
if [[ "$INVALID_OUTPUT" != *'artifact "raw_result" without a non-empty string path'* ]]; then
    echo "expected clear invalid artifact error" >&2
    echo "$INVALID_OUTPUT" >&2
    exit 1
fi

echo "Node.js bench artifacts smoke passed."
