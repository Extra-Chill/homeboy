#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-node-bench-warmup.XXXXXX")"
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

make_project() {
    local name="$1"
    local project_dir="$TMP_DIR/$name"
    mkdir -p "$project_dir/bench"
    printf '{"name":"%s"}\n' "$name" > "$project_dir/package.json"
    cat > "$project_dir/bench/count.bench.mjs" <<'EOF'
let calls = 0;
export default async function () {
  calls += 1;
  return { metrics: { call_count: calls } };
}
EOF
    printf '%s\n' "$project_dir"
}

run_runner() {
    local project_dir="$1"
    local results_file="$2"
    local warmup_value="${3-__unset__}"
    if [ "$warmup_value" = "__unset__" ]; then
        HOMEBOY_RUNTIME_BENCH_HELPER_JS="$HELPER_JS" \
        HOMEBOY_COMPONENT_PATH="$project_dir" \
        HOMEBOY_COMPONENT_ID="node-bench-warmup-smoke" \
        HOMEBOY_BENCH_ITERATIONS="1" \
        HOMEBOY_BENCH_RESULTS_FILE="$results_file" \
            node "$SCRIPT_DIR/bench-runner.mjs"
    else
        HOMEBOY_RUNTIME_BENCH_HELPER_JS="$HELPER_JS" \
        HOMEBOY_COMPONENT_PATH="$project_dir" \
        HOMEBOY_COMPONENT_ID="node-bench-warmup-smoke" \
        HOMEBOY_BENCH_ITERATIONS="1" \
        HOMEBOY_BENCH_WARMUP_ITERATIONS="$warmup_value" \
        HOMEBOY_BENCH_RESULTS_FILE="$results_file" \
            node "$SCRIPT_DIR/bench-runner.mjs"
    fi
}

DEFAULT_PROJECT="$(make_project default-warmup)"
DEFAULT_RESULTS="$TMP_DIR/default-results.json"
run_runner "$DEFAULT_PROJECT" "$DEFAULT_RESULTS" >/dev/null
assert_json "$DEFAULT_RESULTS" '
const metric = data.scenarios[0].metrics.call_count;
if (metric !== 2) throw new Error(`default warmup should discard one run; got ${metric}`);
'

ZERO_PROJECT="$(make_project zero-warmup)"
ZERO_RESULTS="$TMP_DIR/zero-results.json"
run_runner "$ZERO_PROJECT" "$ZERO_RESULTS" 0 >/dev/null
assert_json "$ZERO_RESULTS" '
const metric = data.scenarios[0].metrics.call_count;
if (metric !== 1) throw new Error(`zero warmup should run only measured iteration; got ${metric}`);
'

POSITIVE_PROJECT="$(make_project positive-warmup)"
POSITIVE_RESULTS="$TMP_DIR/positive-results.json"
run_runner "$POSITIVE_PROJECT" "$POSITIVE_RESULTS" 2 >/dev/null
assert_json "$POSITIVE_RESULTS" '
const metric = data.scenarios[0].metrics.call_count;
if (metric !== 3) throw new Error(`two warmups should discard two runs; got ${metric}`);
'

NEGATIVE_PROJECT="$(make_project negative-warmup)"
NEGATIVE_RESULTS="$TMP_DIR/negative-results.json"
run_runner "$NEGATIVE_PROJECT" "$NEGATIVE_RESULTS" -3 >/dev/null
assert_json "$NEGATIVE_RESULTS" '
const metric = data.scenarios[0].metrics.call_count;
if (metric !== 1) throw new Error(`negative warmup should clamp to zero; got ${metric}`);
'

INVALID_PROJECT="$(make_project invalid-warmup)"
set +e
INVALID_OUTPUT="$(run_runner "$INVALID_PROJECT" "$TMP_DIR/invalid-results.json" nope 2>&1)"
INVALID_EXIT=$?
set -e
if [ "$INVALID_EXIT" -eq 0 ]; then
    echo "expected non-integer warmup value to fail" >&2
    exit 1
fi
if [[ "$INVALID_OUTPUT" != *'HOMEBOY_BENCH_WARMUP_ITERATIONS must be an integer, got "nope"'* ]]; then
    echo "expected clear warmup parse error" >&2
    echo "$INVALID_OUTPUT" >&2
    exit 1
fi

LIST_PROJECT="$(make_project list-only-invalid-warmup)"
LIST_RESULTS="$TMP_DIR/list-results.json"
HOMEBOY_RUNTIME_BENCH_HELPER_JS="$HELPER_JS" \
HOMEBOY_COMPONENT_PATH="$LIST_PROJECT" \
HOMEBOY_COMPONENT_ID="node-bench-warmup-smoke" \
HOMEBOY_BENCH_ITERATIONS="1" \
HOMEBOY_BENCH_WARMUP_ITERATIONS="nope" \
HOMEBOY_BENCH_LIST_ONLY="1" \
HOMEBOY_BENCH_RESULTS_FILE="$LIST_RESULTS" \
    node "$SCRIPT_DIR/bench-runner.mjs" >/dev/null
assert_json "$LIST_RESULTS" '
if (data.iterations !== 0) throw new Error(`list mode should not run iterations; got ${data.iterations}`);
if (data.scenarios.length !== 1) throw new Error(`list mode should discover one scenario; got ${data.scenarios.length}`);
'

echo "Node.js bench warmup smoke passed."
