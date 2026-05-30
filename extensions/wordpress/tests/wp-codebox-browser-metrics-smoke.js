'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const {
  enrichBenchResultsWithBrowserMetrics,
  parseWpCodeboxBrowserArtifacts,
} = require('../lib/wp-codebox-browser-metrics');

function writeJson(filePath, value) {
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, `${JSON.stringify(value, null, 2)}\n`);
}

function writeJsonl(filePath, rows) {
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, `${rows.map((row) => JSON.stringify(row)).join('\n')}\n`);
}

function withTempDirectory(callback) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'wp-codebox-browser-metrics-'));
  try {
    callback(root);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
}

withTempDirectory((root) => {
  const browser = path.join(root, 'artifact-123', 'files', 'browser');
  writeJson(path.join(browser, 'summary.json'), {
    peakUsedJSHeapSize: 9000,
    finalUsedJSHeapSize: 7000,
    postIdleUsedJSHeapSize: 6500,
    generationHeapDeltaBytes: 4500,
    domNodeCount: 42,
    iframeCount: 3,
  });
  writeJson(path.join(browser, 'memory.json'), {
    samples: [
      { label: 'before_generation', usedJSHeapSize: 2500 },
      { label: 'after_generation', usedJSHeapSize: 7000 },
      { label: 'post_idle', usedJSHeapSize: 6500 },
    ],
  });
  writeJson(path.join(browser, 'performance.json'), {
    resources: [
      { name: 'editor.js', transferSize: 1000 },
      { name: 'style.css', transferSize: 250 },
    ],
    longTasks: [
      { duration: 50.5 },
      { duration: 12.25 },
    ],
  });
  writeJsonl(path.join(browser, 'checkpoints.jsonl'), [
    { label: 'load', usedJSHeapSize: 3000 },
    { label: 'post_idle', usedJSHeapSize: 6500 },
  ]);

  const parsed = parseWpCodeboxBrowserArtifacts(root);
  assert.deepEqual(parsed.metrics, {
    browser_peak_used_js_heap_bytes: 9000,
    browser_final_used_js_heap_bytes: 7000,
    browser_post_idle_used_js_heap_bytes: 6500,
    browser_generation_heap_delta_bytes: 4500,
    browser_dom_node_count: 42,
    browser_iframe_count: 3,
    browser_resource_count: 2,
    browser_transfer_size_bytes: 1250,
    browser_long_task_count: 2,
    browser_long_task_total_ms: 62.75,
  });
  assert.deepEqual(parsed.artifacts, {
    browser_summary: { path: 'artifact-123/files/browser/summary.json', kind: 'json' },
    browser_memory: { path: 'artifact-123/files/browser/memory.json', kind: 'json' },
    browser_performance: { path: 'artifact-123/files/browser/performance.json', kind: 'json' },
    browser_checkpoints: { path: 'artifact-123/files/browser/checkpoints.jsonl', kind: 'jsonl' },
  });

  const enriched = enrichBenchResultsWithBrowserMetrics({
    component_id: 'fixture',
    scenarios: [
      { id: '__bootstrap', metrics: { boot_ms: 1 } },
      { id: 'editor-generation', metrics: { mean_ms: 123 }, artifacts: { transcript: { path: 'transcript.json' } } },
    ],
  }, root);
  assert.equal(enriched.metrics.browser_peak_used_js_heap_bytes, 9000);
  assert.equal(enriched.scenarios[0].metrics.browser_peak_used_js_heap_bytes, undefined);
  assert.equal(enriched.scenarios[1].metrics.browser_peak_used_js_heap_bytes, 9000);
  assert.equal(enriched.scenarios[1].artifacts.browser_summary.path, 'artifact-123/files/browser/summary.json');
  assert.equal(enriched.scenarios[1].artifacts.transcript.path, 'transcript.json');
});

withTempDirectory((root) => {
  const result = parseWpCodeboxBrowserArtifacts(root);
  assert.deepEqual(result, { metrics: {}, artifacts: {} });

  const benchResults = { component_id: 'fixture', scenarios: [{ id: 'one', metrics: { mean_ms: 1 } }] };
  assert.deepEqual(enrichBenchResultsWithBrowserMetrics(benchResults, root), benchResults);
});

withTempDirectory((root) => {
  const browser = path.join(root, 'files', 'browser');
  writeJson(path.join(browser, 'memory.json'), {
    samples: [
      { label: 'initial', usedJSHeapSize: 100 },
      { label: 'after_generation', usedJSHeapSize: 250 },
      { label: 'post_idle', usedJSHeapSize: 175, domNodeCount: 9 },
    ],
  });

  const parsed = parseWpCodeboxBrowserArtifacts(root);
  assert.equal(parsed.metrics.browser_peak_used_js_heap_bytes, 250);
  assert.equal(parsed.metrics.browser_final_used_js_heap_bytes, 175);
  assert.equal(parsed.metrics.browser_post_idle_used_js_heap_bytes, 175);
  assert.equal(parsed.metrics.browser_dom_node_count, 9);
  assert.deepEqual(Object.keys(parsed.artifacts), ['browser_memory']);
});

withTempDirectory((root) => {
  const browser = path.join(root, 'files', 'browser');
  writeJson(path.join(browser, 'summary.json'), { peakUsedJSHeapSize: 12 });
  const benchResultsPath = path.join(root, 'bench-results.json');
  writeJson(benchResultsPath, { scenarios: [{ id: 'scenario', metrics: {} }] });

  const result = spawnSync(process.execPath, [path.join(__dirname, '..', 'lib', 'wp-codebox-browser-metrics.js'), benchResultsPath, root], {
    encoding: 'utf8',
  });
  assert.equal(result.status, 0, result.stderr);
  const enriched = JSON.parse(result.stdout);
  assert.equal(enriched.scenarios[0].metrics.browser_peak_used_js_heap_bytes, 12);
});

console.log('✓ WP Codebox browser metrics smoke test PASSED');
