#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-node-browser-page-scenario.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

HELPER_UNDER_TEST="$SCRIPT_DIR/browser-helper.mjs" node --input-type=module - "$TMP_DIR" <<'EOF'
import { readFile } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { join } from 'node:path';

const tmpDir = process.argv[2];
const { runBrowserPageScenario } = await import(process.env.HELPER_UNDER_TEST);

function makePage(status = 200) {
  return {
    gotoCalls: [],
    selectorCalls: [],
    textCalls: [],
    async goto(target, options) {
      this.gotoCalls.push({ target, options });
      return { status: () => status };
    },
    async waitForSelector(selector, options) {
      this.selectorCalls.push({ selector, options });
    },
    getByText(text, options) {
      this.textCalls.push({ text, options });
      return { waitFor: async () => {} };
    },
    async title() {
      return 'Scenario smoke page';
    },
  };
}

async function fakeBrowserBench(options) {
  const page = makePage(options.status ?? 200);
  const marks = [];
  await options.action({
    page,
    mark: async (name) => marks.push(name),
  });
  return {
    metrics: { custom_ms: 12, mark_count: marks.length },
    artifacts: {
      report: { path: join(tmpDir, 'report.json'), kind: 'json', label: 'Report' },
    },
  };
}

const success = await runBrowserPageScenario({
  id: 'page-scenario-success',
  artifactDir: tmpDir,
  target: 'https://example.test/page',
  browserBench: fakeBrowserBench,
  metadata: { suite: 'smoke' },
  assertions: [
    { type: 'status', expected: 200 },
    { type: 'selector', selector: 'main' },
    { type: 'text', text: 'Ready' },
    { type: 'title', includes: 'smoke' },
    { type: 'artifact', key: 'report', kind: 'json' },
  ],
  action: async ({ response, target }) => {
    if (response.status() !== 200) throw new Error('action did not receive navigation response');
    if (target !== 'https://example.test/page') throw new Error('action did not receive target');
  },
  sanitizeArtifacts: async ({ artifacts }) => ({
    ...artifacts,
    report: { ...artifacts.report, path: '<sanitized-report-path>' },
  }),
  postSanitizeAssertions: [{ type: 'artifact', key: 'rawResult', kind: 'browser-page-scenario-result' }],
});

if (success.metrics.custom_ms !== 12) throw new Error('success metrics were not preserved');
if (success.artifacts.report.path !== '<sanitized-report-path>') throw new Error('sanitizeArtifacts did not rewrite artifact');
if (!success.artifacts.rawResult) throw new Error('raw result artifact missing');
if (!existsSync(success.artifacts.rawResult.path)) throw new Error('raw result file missing');
const rawResult = JSON.parse(await readFile(success.artifacts.rawResult.path, 'utf8'));
if (rawResult.id !== 'page-scenario-success') throw new Error('raw result id missing');
if (rawResult.artifacts.report.kind !== 'json') throw new Error('raw result did not propagate bench artifact');

let failed = false;
try {
  await runBrowserPageScenario({
    id: 'page-scenario-failure',
    artifactDir: tmpDir,
    target: 'https://example.test/fail',
    browserBench: async (options) => {
      const page = makePage(500);
      await options.action({ page, mark: async () => {} });
      return { metrics: {}, artifacts: {} };
    },
    assertions: [{ type: 'status', expected: 200 }],
  });
} catch (err) {
  failed = err.message.includes('Browser page scenario assertion failed') && err.message.includes('Expected page status 200');
}
if (!failed) throw new Error('assertion failure did not produce expected error');

const propagated = await runBrowserPageScenario({
  id: 'page-scenario-artifacts',
  artifactDir: tmpDir,
  browserBench: fakeBrowserBench,
  assertions: [{ type: 'artifact', key: 'report', kind: 'json' }],
});
if (propagated.artifacts.report.kind !== 'json') throw new Error('bench artifact was not propagated');
if (!propagated.artifacts.rawResult.path.endsWith('page-scenario-artifacts-raw-result.json')) throw new Error('raw result artifact path not stable');
EOF

echo "Node.js browser page scenario smoke passed."
