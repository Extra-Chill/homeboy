#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-node-bench-metrics.XXXXXX")"
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
    node -e "const fs = require('fs'); const data = JSON.parse(fs.readFileSync(process.argv[1], 'utf8')); ${script}" "$file"
}

run_runner() {
    local project_dir="$1"
    local results_file="$2"
    HOMEBOY_RUNTIME_BENCH_HELPER_JS="$HELPER_JS" \
        HOMEBOY_COMPONENT_PATH="$project_dir" \
        HOMEBOY_COMPONENT_ID="node-bench-smoke" \
        HOMEBOY_BENCH_ITERATIONS="3" \
        HOMEBOY_BENCH_RESULTS_FILE="$results_file" \
        node "$SCRIPT_DIR/bench-runner.mjs"
}

make_project() {
    local name="$1"
    local project_dir="$TMP_DIR/$name"
    mkdir -p "$project_dir/bench"
    printf '{"name":"%s"}\n' "$name" > "$project_dir/package.json"
    printf '%s\n' "$project_dir"
}

VOID_PROJECT="$(make_project void-return)"
cat > "$VOID_PROJECT/bench/void.bench.mjs" <<'EOF'
export default async function () {}
EOF

VOID_RESULTS="$TMP_DIR/void-results.json"
run_runner "$VOID_PROJECT" "$VOID_RESULTS" >/dev/null
assert_json "$VOID_RESULTS" '
if (data.scenarios.length !== 1) throw new Error("expected one void scenario");
const metrics = data.scenarios[0].metrics;
for (const key of ["mean_ms", "p50_ms", "p95_ms", "p99_ms", "min_ms", "max_ms"]) {
  if (typeof metrics[key] !== "number") throw new Error(`missing timing metric ${key}`);
}
if (Object.prototype.hasOwnProperty.call(metrics, "turn_count")) throw new Error("void workload emitted custom metric");
'

CUSTOM_PROJECT="$(make_project custom-metrics)"
cat > "$CUSTOM_PROJECT/bench/custom.bench.mjs" <<'EOF'
let i = 0;
export default async function () {
  i += 1;
  return { metrics: { turn_count: i, score: i + 1 }, artifacts: { ignored: "for-now" } };
}
EOF

CUSTOM_RESULTS="$TMP_DIR/custom-results.json"
run_runner "$CUSTOM_PROJECT" "$CUSTOM_RESULTS" >/dev/null
assert_json "$CUSTOM_RESULTS" '
const metrics = data.scenarios[0].metrics;
if (metrics.turn_count !== 3) throw new Error(`expected mean turn_count 3, got ${metrics.turn_count}`);
if (metrics.score !== 4) throw new Error(`expected mean score 4, got ${metrics.score}`);
if (typeof metrics.p95_ms !== "number") throw new Error("timing metrics missing beside custom metrics");
'

INVALID_PROJECT="$(make_project invalid-metric)"
cat > "$INVALID_PROJECT/bench/invalid.bench.mjs" <<'EOF'
export default async function () {
  return { metrics: { turn_count: "three" } };
}
EOF

set +e
INVALID_OUTPUT="$(run_runner "$INVALID_PROJECT" "$TMP_DIR/invalid-results.json" 2>&1)"
INVALID_EXIT=$?
set -e
if [ "$INVALID_EXIT" -eq 0 ]; then
    echo "expected non-number metric workload to fail" >&2
    exit 1
fi
if [[ "$INVALID_OUTPUT" != *'metric "turn_count" with non-finite numeric value'* ]]; then
    echo "expected clear non-number metric error" >&2
    echo "$INVALID_OUTPUT" >&2
    exit 1
fi

COLLISION_PROJECT="$(make_project collision-metric)"
cat > "$COLLISION_PROJECT/bench/collision.bench.mjs" <<'EOF'
export default async function () {
  return { metrics: { p95_ms: 123 } };
}
EOF

set +e
COLLISION_OUTPUT="$(run_runner "$COLLISION_PROJECT" "$TMP_DIR/collision-results.json" 2>&1)"
COLLISION_EXIT=$?
set -e
if [ "$COLLISION_EXIT" -eq 0 ]; then
    echo "expected timing metric collision workload to fail" >&2
    exit 1
fi
if [[ "$COLLISION_OUTPUT" != *'metric "p95_ms", which is owned by the bench dispatcher'* ]]; then
    echo "expected clear timing metric collision error" >&2
    echo "$COLLISION_OUTPUT" >&2
    exit 1
fi

echo "Node.js bench custom metrics smoke passed."
