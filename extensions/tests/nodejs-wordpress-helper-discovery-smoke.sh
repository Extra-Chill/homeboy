#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HOMEBOY_CORE_DIR="${HOMEBOY_CORE_DIR:-$(cd "${ROOT_DIR}/.." && pwd)/homeboy}"
BENCH_HELPER="${HOMEBOY_RUNTIME_BENCH_HELPER_JS:-${HOMEBOY_CORE_DIR}/src/core/extension/runtime/bench-helper.mjs}"
BASH_PREFLIGHT_HELPER="${HOMEBOY_RUNTIME_BASH_PREFLIGHT:-${HOMEBOY_CORE_DIR}/src/core/extension/runtime/bash-preflight.sh}"

if [ ! -f "$BENCH_HELPER" ]; then
    echo "Missing required file: $BENCH_HELPER" >&2
    exit 1
fi

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

PROJECT_DIR="$TMP_DIR/project"
RESULTS_FILE="$TMP_DIR/results.json"
mkdir -p "$PROJECT_DIR/bench" "$PROJECT_DIR/node_modules/.bin"

cat > "$PROJECT_DIR/node_modules/.bin/tsx" <<'EOF'
#!/usr/bin/env bash
exec node "$@"
EOF
chmod +x "$PROJECT_DIR/node_modules/.bin/tsx"

cat > "$PROJECT_DIR/package.json" <<'EOF'
{
  "name": "nodejs-wordpress-helper-discovery-smoke",
  "private": true,
  "type": "module"
}
EOF

cat > "$PROJECT_DIR/bench/wordpress-helper-discovery.bench.mjs" <<'EOF'
import assert from 'node:assert';
import { existsSync } from 'node:fs';

export default async function () {
    const manifestPath = process.env.HOMEBOY_WORDPRESS_HELPER_MANIFEST;
    assert.ok(manifestPath, 'HOMEBOY_WORDPRESS_HELPER_MANIFEST is set');

    const { getWordPressHelperManifest } = await import(manifestPath);
    const manifest = getWordPressHelperManifest();

    assert.equal(manifest.version, 1);
    assert.equal(process.env.HOMEBOY_WORDPRESS_REQUEST_PROFILER_HELPER, manifest.helpers.requestProfiler);
    assert.equal(process.env.HOMEBOY_WORDPRESS_TIMING_CORRELATOR_HELPER, manifest.helpers.timingCorrelator);
    assert.equal(process.env.HOMEBOY_WORDPRESS_BOOTSTRAP_TIMELINE_HELPER, manifest.helpers.bootstrapTimeline);
    assert.ok(manifest.helpers.pageProfiler, 'page profiler helper is published');
    assert.ok(manifest.helpers.adminPageScenarios, 'admin page scenarios helper is published');

    for (const helperPath of Object.values(manifest.helpers)) {
        assert.ok(existsSync(helperPath), `helper exists at ${helperPath}`);
    }

    return {
        metrics: { helper_count: Object.keys(manifest.helpers).length },
        metadata: { wordpress_helper_manifest: manifestPath },
    };
}
EOF

HOMEBOY_RUNTIME_BENCH_HELPER_JS="$BENCH_HELPER" \
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_COMPONENT_ID="nodejs-wordpress-helper-discovery-smoke" \
HOMEBOY_BENCH_RESULTS_FILE="$RESULTS_FILE" \
HOMEBOY_BENCH_ITERATIONS=1 \
HOMEBOY_BENCH_WARMUP_ITERATIONS=0 \
HOMEBOY_RUNTIME_BASH_PREFLIGHT="$BASH_PREFLIGHT_HELPER" \
    bash "$ROOT_DIR/nodejs/scripts/bench/bench-runner.sh" >/dev/null

node --input-type=module - "$RESULTS_FILE" <<'NODE'
import { readFileSync } from 'node:fs';

const results = JSON.parse(readFileSync(process.argv[2], 'utf8'));
const scenario = results.scenarios?.[0];
if (!scenario) throw new Error('missing scenario');
if (scenario.metrics?.helper_count !== 12) {
    throw new Error(`helper count metric regressed: ${JSON.stringify(scenario.metrics)}`);
}
if (!scenario.metadata?.wordpress_helper_manifest?.endsWith('/wordpress/lib/helper-manifest.js')) {
    throw new Error(`manifest metadata missing: ${JSON.stringify(scenario.metadata)}`);
}
NODE

echo "nodejs wordpress helper discovery smoke passed"
