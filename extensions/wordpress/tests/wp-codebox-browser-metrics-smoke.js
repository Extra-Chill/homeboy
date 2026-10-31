'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const {
  enrichBenchResultsWithBrowserMetrics,
  runWpCodeboxBrowserMetrics,
} = require('../lib/wp-codebox-browser-metrics');

function writeJson(filePath, value) {
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, `${JSON.stringify(value, null, 2)}\n`);
}

function withTempDirectory(callback) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'wp-codebox-browser-metrics-'));
  try {
    callback(root);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
}

function writeFakeWpCodebox(filePath, output) {
  fs.writeFileSync(filePath, `#!/usr/bin/env node
const fs = require('node:fs');
const args = process.argv.slice(2);
if (args.includes('--version')) { process.stdout.write('0.21.0'); process.exit(0); }
if (args.slice(-3).join(' ') === 'runtime descriptor --json') { process.stdout.write(JSON.stringify({ schema: 'wp-codebox/runtime-descriptor/v1', readiness: { status: 'available', browserRuntime: { status: 'ready' } }, contractManifest: { schemas: { runtimeBoundary: { browserContainedSiteOpen: 'wp-codebox/browser-contained-site-open/v1' } } } })); process.exit(0); }
fs.writeFileSync(process.env.FAKE_WP_CODEBOX_ARGS_PATH, JSON.stringify(args));
if (args[0] !== 'artifacts' || args[1] !== 'browser-metrics' || !args.includes('--json')) {
  console.error('unexpected wp-codebox args: ' + JSON.stringify(args));
  process.exit(2);
}
process.stdout.write(${JSON.stringify(`${JSON.stringify(output, null, 2)}\n`)});
`);
  fs.chmodSync(filePath, 0o755);
}

withTempDirectory((root) => {
  const artifactsDirectory = path.join(root, 'artifact-bundle');
  const argsPath = path.join(root, 'wp-codebox-args.json');
  const fakeWpCodebox = path.join(root, 'wp-codebox.js');
  const cliOutput = {
    schema: 'wp-codebox/browser-metrics/v1',
    bundleDirectory: artifactsDirectory,
    hasBrowserMetrics: true,
    metrics: {
      browser_peak_used_js_heap_bytes: 9000,
      browser_final_used_js_heap_bytes: 7000,
      browser_checkpoint_count: 4,
      browser_dom_node_count: 42,
      browser_iframe_count: 3,
      browser_resource_count: 2,
      browser_transfer_size_bytes: 1250,
      browser_long_task_count: 2,
      browser_long_task_total_ms: 62.75,
    },
    artifacts: {
      summary: { path: 'files/browser/summary.json', kind: 'json' },
      memory: { path: 'files/browser/memory.json', kind: 'json' },
      performance: { path: 'files/browser/performance.json', kind: 'json' },
      checkpoints: { path: 'files/browser/checkpoints.jsonl', kind: 'jsonl' },
      html: { path: 'files/browser/snapshot.html', kind: 'html' },
      screenshot: { path: 'files/browser/screenshot.png', kind: 'png' },
    },
  };
  writeFakeWpCodebox(fakeWpCodebox, cliOutput);

  const previousArgsPath = process.env.FAKE_WP_CODEBOX_ARGS_PATH;
  process.env.FAKE_WP_CODEBOX_ARGS_PATH = argsPath;
  try {
    const parsed = runWpCodeboxBrowserMetrics(artifactsDirectory, fakeWpCodebox);
    assert.deepEqual(JSON.parse(fs.readFileSync(argsPath, 'utf8')), [
      'artifacts',
      'browser-metrics',
      '--bundle',
      artifactsDirectory,
      '--json',
    ]);
    assert.deepEqual(parsed.metrics, cliOutput.metrics);
    assert.deepEqual(parsed.artifacts, {
      browser_summary: { path: 'files/browser/summary.json', kind: 'json' },
      browser_memory: { path: 'files/browser/memory.json', kind: 'json' },
      browser_performance: { path: 'files/browser/performance.json', kind: 'json' },
      browser_checkpoints: { path: 'files/browser/checkpoints.jsonl', kind: 'jsonl' },
      browser_html: { path: 'files/browser/snapshot.html', kind: 'html' },
      browser_screenshot: { path: 'files/browser/screenshot.png', kind: 'png' },
    });

    const enriched = enrichBenchResultsWithBrowserMetrics({
      component_id: 'fixture',
      scenarios: [
        { id: '__bootstrap', metrics: { boot_ms: 1 } },
        { id: 'editor-generation', metrics: { mean_ms: 123 }, artifacts: { transcript: { path: 'transcript.json' } } },
      ],
    }, artifactsDirectory, fakeWpCodebox);
    assert.equal(enriched.metrics.browser_peak_used_js_heap_bytes, 9000);
    assert.equal(enriched.scenarios[0].metrics.browser_peak_used_js_heap_bytes, undefined);
    assert.equal(enriched.scenarios[1].metrics.browser_peak_used_js_heap_bytes, 9000);
    assert.equal(enriched.scenarios[1].artifacts.browser_summary.path, 'files/browser/summary.json');
    assert.equal(enriched.scenarios[1].artifacts.browser_screenshot.path, 'files/browser/screenshot.png');
    assert.equal(enriched.scenarios[1].artifacts.transcript.path, 'transcript.json');
  } finally {
    if (previousArgsPath === undefined) {
      delete process.env.FAKE_WP_CODEBOX_ARGS_PATH;
    } else {
      process.env.FAKE_WP_CODEBOX_ARGS_PATH = previousArgsPath;
    }
  }
});

withTempDirectory((root) => {
  const artifactsDirectory = path.join(root, 'empty-artifact-bundle');
  const fakeWpCodebox = path.join(root, 'wp-codebox');
  const argsPath = path.join(root, 'wp-codebox-args.json');
  writeFakeWpCodebox(fakeWpCodebox, {
    schema: 'wp-codebox/browser-metrics/v1',
    bundleDirectory: artifactsDirectory,
    hasBrowserMetrics: false,
    metrics: {},
    artifacts: {},
  });

  const previousArgsPath = process.env.FAKE_WP_CODEBOX_ARGS_PATH;
  process.env.FAKE_WP_CODEBOX_ARGS_PATH = argsPath;
  try {
    const benchResults = { component_id: 'fixture', scenarios: [{ id: 'one', metrics: { mean_ms: 1 } }] };
    assert.deepEqual(enrichBenchResultsWithBrowserMetrics(benchResults, artifactsDirectory, fakeWpCodebox), benchResults);
  } finally {
    if (previousArgsPath === undefined) {
      delete process.env.FAKE_WP_CODEBOX_ARGS_PATH;
    } else {
      process.env.FAKE_WP_CODEBOX_ARGS_PATH = previousArgsPath;
    }
  }
});

withTempDirectory((root) => {
  const artifactsDirectory = path.join(root, 'artifact-bundle');
  const fakeWpCodebox = path.join(root, 'wp-codebox');
  const argsPath = path.join(root, 'wp-codebox-args.json');
  writeFakeWpCodebox(fakeWpCodebox, {
    schema: 'wp-codebox/browser-metrics/v1',
    bundleDirectory: artifactsDirectory,
    hasBrowserMetrics: true,
    metrics: { browser_peak_used_js_heap_bytes: 12 },
    artifacts: {},
  });
  const benchResultsPath = path.join(root, 'bench-results.json');
  writeJson(benchResultsPath, { scenarios: [{ id: 'scenario', metrics: {} }] });

  const result = spawnSync(process.execPath, [path.join(__dirname, '..', 'lib', 'wp-codebox-browser-metrics.js'), benchResultsPath, artifactsDirectory, fakeWpCodebox], {
    encoding: 'utf8',
    env: { ...process.env, FAKE_WP_CODEBOX_ARGS_PATH: argsPath },
  });
  assert.equal(result.status, 0, result.stderr);
  const enriched = JSON.parse(result.stdout);
  assert.equal(enriched.scenarios[0].metrics.browser_peak_used_js_heap_bytes, 12);
});

console.log('✓ WP Codebox browser metrics smoke test PASSED');
