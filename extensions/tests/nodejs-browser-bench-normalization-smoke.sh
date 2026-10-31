#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HOMEBOY_CORE_DIR="${HOMEBOY_CORE_DIR:-$(cd "${ROOT_DIR}/.." && pwd)/homeboy}"
BENCH_HELPER="${HOMEBOY_RUNTIME_BENCH_HELPER_JS:-${HOMEBOY_CORE_DIR}/src/core/extension/runtime/bench-helper.mjs}"

if [ ! -f "$BENCH_HELPER" ]; then
    echo "Missing required file: $BENCH_HELPER" >&2
    exit 1
fi

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

PROJECT_DIR="$TMP_DIR/project"
RESULTS_FILE="$TMP_DIR/results.json"
mkdir -p "$PROJECT_DIR/bench"

cat > "$PROJECT_DIR/bench/browser-homepage.bench.mjs" <<'EOF'
export default async function () {
    return {
        browserMetrics: {
            browser_ready_ms: 12.3456,
            ignored_nan: Number.NaN,
        },
        browserArtifacts: {
            trace: { path: 'trace.zip', kind: 'playwright-trace', extra: 'dropped' },
        },
        rawResultArtifact: { path: 'raw-result.json', kind: 'json', label: 'Raw result' },
        metadata: { route: '/homepage' },
    };
}
EOF

HOMEBOY_RUNTIME_BENCH_HELPER_JS="$BENCH_HELPER" \
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_COMPONENT_ID="nodejs-browser-normalization-smoke" \
HOMEBOY_BENCH_RESULTS_FILE="$RESULTS_FILE" \
HOMEBOY_BENCH_ITERATIONS=1 \
HOMEBOY_BENCH_WARMUP_ITERATIONS=0 \
    node "$ROOT_DIR/nodejs/scripts/bench/bench-runner.mjs" >/dev/null

node --input-type=module - "$RESULTS_FILE" <<'NODE'
import { readFileSync } from 'node:fs';

const results = JSON.parse(readFileSync(process.argv[2], 'utf8'));
const scenario = results.scenarios?.[0];
if (!scenario) {
    throw new Error('missing scenario');
}
if (scenario.metrics?.browser_ready_ms !== 12.346) {
    throw new Error(`browser metric was not normalized: ${JSON.stringify(scenario.metrics)}`);
}
if ('ignored_nan' in scenario.metrics) {
    throw new Error(`non-finite browser metric was retained: ${JSON.stringify(scenario.metrics)}`);
}
if (scenario.metadata?.browser_evidence_schema !== 'homeboy/browser-evidence/v1') {
    throw new Error(`browser evidence schema was not persisted: ${JSON.stringify(scenario.metadata)}`);
}
if (scenario.metadata?.route !== '/homepage') {
    throw new Error(`browser metadata was not preserved: ${JSON.stringify(scenario.metadata)}`);
}
if (scenario.artifacts?.trace?.kind !== 'playwright-trace' || scenario.artifacts?.trace?.path !== 'trace.zip') {
    throw new Error(`browser artifact was not normalized: ${JSON.stringify(scenario.artifacts)}`);
}
if ('extra' in scenario.artifacts.trace) {
    throw new Error(`browser artifact kept unsupported fields: ${JSON.stringify(scenario.artifacts.trace)}`);
}
if (scenario.artifacts?.raw_result?.label !== 'Raw result') {
    throw new Error(`raw result artifact was not normalized: ${JSON.stringify(scenario.artifacts)}`);
}
NODE

echo "nodejs browser bench normalization smoke passed"
