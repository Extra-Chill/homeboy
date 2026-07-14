#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-node-browser-bench.XXXXXX")"
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
mkdir -p "$PROJECT_DIR/bench" "$PROJECT_DIR/artifacts"
cat > "$PROJECT_DIR/package.json" <<'EOF'
{"name":"homeboy-node-browser-bench-smoke","private":true,"type":"module"}
EOF

NO_DEPS_PROJECT="$TMP_DIR/no-deps-project"
mkdir -p "$NO_DEPS_PROJECT"
cat > "$NO_DEPS_PROJECT/package.json" <<'EOF'
{"name":"homeboy-node-browser-bench-no-deps","private":true}
EOF

set +e
MISSING_DEPS_OUTPUT="$(HOMEBOY_NODEJS_PLAYWRIGHT_RUNTIME_DIR="$TMP_DIR/empty-runtime" HOMEBOY_COMPONENT_PATH="$NO_DEPS_PROJECT" HELPER_UNDER_TEST="$SCRIPT_DIR/browser-helper.mjs" node --input-type=module - <<'EOF' 2>&1
const { runBrowserBench } = await import(process.env.HELPER_UNDER_TEST);
try {
  await runBrowserBench({ action: async () => {} });
} catch (err) {
  console.error(err.message);
  process.exit(42);
}
process.exit(0);
EOF
)"
MISSING_DEPS_EXIT=$?
set -e
if [ "$MISSING_DEPS_EXIT" -ne 42 ]; then
    echo "expected missing Playwright dependency probe to fail with guidance" >&2
    echo "$MISSING_DEPS_OUTPUT" >&2
    exit 1
fi
if [[ "$MISSING_DEPS_OUTPUT" != *"homeboy extension action nodejs browser.playwright.setup"* ]]; then
    echo "missing dependency error did not include actionable setup guidance" >&2
    echo "$MISSING_DEPS_OUTPUT" >&2
    exit 1
fi

# Browser launch is an opt-in integration check. It uses the extension-owned
# runtime already prepared on the runner and never materializes project deps.
if [ "${HOMEBOY_RUN_PLAYWRIGHT_INTEGRATION:-0}" != "1" ]; then
    echo "Node.js browser helper integration smoke skipped (set HOMEBOY_RUN_PLAYWRIGHT_INTEGRATION=1 to run)."
    exit 0
fi

test ! -e "$PROJECT_DIR/node_modules" || { echo "integration project must not have node_modules" >&2; exit 1; }

cat > "$PROJECT_DIR/bench/browser.bench.mjs" <<'EOF'
import { createServer } from 'node:http';

const { runBrowserBench } = await import(process.env.HOMEBOY_NODEJS_BROWSER_BENCH_HELPER);

function listen(server) {
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => resolve(server.address().port));
  });
}

export default async function () {
  const server = createServer((req, res) => {
    if (req.url === '/') {
      res.writeHead(200, { 'content-type': 'text/html' });
      res.end(`<!doctype html>
        <title>Browser bench smoke</title>
        <script>console.log('browser-helper-smoke'); fetch('/slow');</script>
        <h1>Browser helper smoke</h1>
        <img src="/missing.png" alt="missing image">
      `);
      return;
    }
    if (req.url === '/slow') {
      setTimeout(() => {
        res.writeHead(200, { 'content-type': 'application/json' });
        res.end('{"ok":true}');
      }, 25);
      return;
    }
    res.writeHead(404, { 'content-type': 'text/plain' });
    res.end('not found');
  });

  const port = await listen(server);
  try {
    return await runBrowserBench({
      id: 'browser-helper-smoke',
      artifactsDir: new URL('../artifacts', import.meta.url).pathname,
      trace: true,
      screenshot: true,
      action: async ({ page, mark }) => {
        await page.goto(`http://127.0.0.1:${port}/`);
        await mark('after_goto');
        await page.getByRole('heading', { name: 'Browser helper smoke' }).waitFor();
        await mark('heading_visible');
      },
    });
  } finally {
    server.close();
  }
}
EOF

RESULTS_FILE="$TMP_DIR/results.json"
HOMEBOY_RUNTIME_BENCH_HELPER_JS="$HELPER_JS" \
HOMEBOY_NODEJS_BROWSER_BENCH_HELPER="$SCRIPT_DIR/browser-helper.mjs" \
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_COMPONENT_ID="node-browser-helper-smoke" \
HOMEBOY_BENCH_ITERATIONS="1" \
HOMEBOY_BENCH_WARMUP_ITERATIONS="0" \
HOMEBOY_BENCH_RESULTS_FILE="$RESULTS_FILE" \
    node "$SCRIPT_DIR/bench-runner.mjs" >/dev/null

node - <<'EOF' "$RESULTS_FILE"
const fs = require('fs');
const path = require('path');
const data = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
if (data.scenarios.length !== 1) throw new Error('expected one scenario');
const scenario = data.scenarios[0];
const metrics = scenario.metrics;
for (const key of [
  'browser_domcontentloaded_ms',
  'browser_load_ms',
  'browser_network_idle_ms',
  'browser_request_count',
  'browser_failed_request_count',
  'browser_slowest_request_ms',
  'after_goto_ms',
  'heading_visible_ms',
]) {
  if (typeof metrics[key] !== 'number' || !Number.isFinite(metrics[key])) {
    throw new Error(`missing numeric metric ${key}`);
  }
}
if (metrics.browser_request_count < 3) throw new Error('expected document, image, and fetch requests');
if (metrics.browser_failed_request_count < 1) throw new Error('expected failed/missing image request');

const artifacts = scenario.artifacts || {};
const expected = {
  trace: 'playwright-trace',
  screenshot: 'screenshot',
  network: 'network-log',
  console: 'console-log',
  browserProfile: 'browser-performance-profile',
  traceSummary: 'browser-trace-summary',
  traceSummaryMarkdown: 'browser-trace-summary-markdown',
};
for (const [key, kind] of Object.entries(expected)) {
  const artifact = artifacts[key];
  if (!artifact) throw new Error(`missing artifact ${key}`);
  if (artifact.kind !== kind) throw new Error(`artifact ${key} had kind ${artifact.kind}, expected ${kind}`);
  if (!fs.existsSync(artifact.path)) throw new Error(`artifact ${key} path does not exist: ${artifact.path}`);
  if (fs.statSync(artifact.path).size <= 0) throw new Error(`artifact ${key} is empty`);
}

const network = JSON.parse(fs.readFileSync(artifacts.network.path, 'utf8'));
if (!network.some((entry) => entry.url.endsWith('/missing.png') && entry.failed)) {
  throw new Error('network log did not record failed image request');
}
const consoleMessages = JSON.parse(fs.readFileSync(artifacts.console.path, 'utf8'));
if (!consoleMessages.some((entry) => entry.text === 'browser-helper-smoke')) {
  throw new Error('console log did not record page console message');
}
if (path.extname(artifacts.trace.path) !== '.zip') throw new Error('trace artifact should be a zip');

const traceSummary = JSON.parse(fs.readFileSync(artifacts.traceSummary.path, 'utf8'));
if (traceSummary.summary.request_count < 3) throw new Error('trace summary did not count browser requests');
if (traceSummary.summary.failed_request_count < 1) throw new Error('trace summary did not count failed requests');
if (!traceSummary.bottlenecks.some((entry) => entry.kind === 'failed-request')) {
  throw new Error('trace summary did not report failed request bottleneck');
}
const traceSummaryMarkdown = fs.readFileSync(artifacts.traceSummaryMarkdown.path, 'utf8');
if (!traceSummaryMarkdown.includes('Browser trace bottlenecks') && !traceSummaryMarkdown.includes('browser trace bottlenecks')) {
  throw new Error('trace summary markdown did not include expected title');
}
for (const key of ['browser_trace_bottleneck_count', 'browser_transfer_bytes', 'browser_console_error_count']) {
  if (typeof metrics[key] !== 'number' || !Number.isFinite(metrics[key])) {
    throw new Error(`missing numeric summary metric ${key}`);
  }
}
EOF

if ! grep -q 'HOMEBOY_NODEJS_BROWSER_BENCH_HELPER' "$SCRIPT_DIR/bench-runner.sh"; then
    echo "bench-runner.sh does not export HOMEBOY_NODEJS_BROWSER_BENCH_HELPER" >&2
    exit 1
fi

echo "Node.js browser helper smoke passed."
