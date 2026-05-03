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

cat > "$PROJECT_DIR/bench/prompt-variant.bench.mjs" <<'EOF'
export default async function () {
    return {
        metrics: { custom_metric: 7 },
        metadata: {
            prompt_variant: 'studio-code',
            prompt_file: 'file:///tmp/prompts/site-build/studio-code.md',
            prompt_category: 'site-build',
        },
    };
}
EOF

HOMEBOY_RUNTIME_BENCH_HELPER_JS="$BENCH_HELPER" \
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_COMPONENT_ID="nodejs-metadata-smoke" \
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
if (scenario.metadata?.prompt_variant !== 'studio-code') {
    throw new Error(`prompt_variant was not persisted: ${JSON.stringify(scenario.metadata)}`);
}
if (scenario.metadata?.prompt_file !== 'file:///tmp/prompts/site-build/studio-code.md') {
    throw new Error(`prompt_file was not persisted: ${JSON.stringify(scenario.metadata)}`);
}
if (scenario.metadata?.prompt_category !== 'site-build') {
    throw new Error(`prompt_category was not persisted: ${JSON.stringify(scenario.metadata)}`);
}
if (scenario.metrics?.custom_metric !== 7) {
    throw new Error(`custom metrics regressed: ${JSON.stringify(scenario.metrics)}`);
}
NODE

echo "nodejs bench metadata smoke passed"
