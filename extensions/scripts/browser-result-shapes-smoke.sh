#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

node - <<'EOF' "$SCRIPT_DIR/lib/browser-result-shapes.cjs"
const assert = require('node:assert/strict');
const shapes = require(process.argv[2]);

const profile = shapes.normalizeBrowserPerformanceProfile({
  page_url: 'https://example.test/app',
  network: [{ url: 'https://example.test/app.js', method: 'get', status: 200, start_time_ms: 1.2345, duration_ms: 5.6789 }],
  phase_marks: [{ name: 'load', start_time_ms: 0 }, { name: 'ready', start_time_ms: 20 }],
  summary: { network_request_count: 1 },
});
assert.equal(profile.schema_version, 1);
assert.equal(profile.network[0].url, 'https://example.test/app.js');
assert.equal(profile.phases.load.duration_ms, 20);

const timing = shapes.normalizeBrowserTiming({
  name: 'https://example.test/wp-json/demo?_wpnonce=secret',
  method: 'post',
  startTime: 10,
  responseStart: 30,
  responseEnd: 50,
}, {
  normalizeUrl: (url) => new URL(url).pathname,
});
assert.equal(timing.normalizedUrl, '/wp-json/demo');
assert.equal(timing.method, 'POST');
assert.equal(timing.ttfbMs, 20);
assert.equal(timing.durationMs, 40);

const rows = shapes.normalizeBrowserProfileTimings({
  resources: [{ name: 'https://example.test/app.js', startTime: 5, responseStart: 8, responseEnd: 12 }],
  network: [{ url: 'https://example.test/app.js', method: 'GET', duration_ms: 9 }],
  phases: { load: { start_time_ms: 0, end_time_ms: 10 } },
}, {
  normalizeUrl: (url) => new URL(url).pathname,
});
assert.equal(rows.length, 1);
assert.equal(rows[0].phase, 'load');

const assertion = shapes.normalizeTraceAssertion('ready', 'pass', 'ready observed', { port: 8881 });
assert.deepEqual(assertion, { data: { port: 8881 }, id: 'ready', message: 'ready observed', status: 'pass' });

const envelope = shapes.normalizeTraceEnvelope({
  component_id: 'nodejs',
  scenario_id: 'browser',
  status: 'pass',
  summary: 'ok',
  artifacts: [{ label: 'profile', path: 'profile.json', kind: 'browser-performance-profile' }],
});
assert.equal(envelope.artifacts[0].kind, 'browser-performance-profile');

console.log('Shared browser result shapes CommonJS smoke passed.');
EOF

node --input-type=module - <<'EOF' "$SCRIPT_DIR/lib/browser-result-shapes.mjs"
import assert from 'node:assert/strict';

const shapes = await import(process.argv[2]);
const artifact = shapes.normalizeBrowserArtifact({ path: 'trace.zip', kind: 'playwright-trace', label: 'Trace' });
assert.deepEqual(artifact, { kind: 'playwright-trace', label: 'Trace', path: 'trace.zip' });

const bottleneck = shapes.normalizeBrowserBottleneck({ kind: 'network', phase: 'load', message: 'slow request', data: { duration_ms: 25 } });
assert.equal(bottleneck.kind, 'network');
assert.equal(bottleneck.data.duration_ms, 25);

console.log('Shared browser result shapes ESM smoke passed.');
EOF
