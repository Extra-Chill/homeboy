import assert from 'node:assert/strict';
import test from 'node:test';

import {
    HOMEBOY_BENCH_RESULTS_SCHEMA,
    HOMEBOY_BROWSER_EVIDENCE_SCHEMA,
    buildBenchResultsEnvelope,
    buildBrowserBenchResult,
} from '../scripts/lib/browser-result-shapes.mjs';

test('browser benchmark helpers emit the canonical Homeboy result envelope', () => {
    const scenario = buildBrowserBenchResult({
        browserResult: {
            metrics: { browser_request_count: 4, browser_load_ms: 98.7654 },
            artifacts: { trace: { path: 'trace.zip', kind: 'playwright-trace' } },
        },
        metrics: { success_rate: 1, skipped: Infinity },
        rawResultArtifact: { path: 'raw-result.json', kind: 'json', label: 'Raw result' },
        artifacts: { screenshot: 'screenshot.png' },
        metadata: { route: '/wp-admin/' },
    });
    const envelope = buildBenchResultsEnvelope({
        componentId: 'example-component',
        iterations: 1,
        scenarios: [{ id: 'browser-homepage', ...scenario }],
    });

    assert.equal(envelope.schema, HOMEBOY_BENCH_RESULTS_SCHEMA);
    assert.deepEqual(envelope.scenarios[0], {
        id: 'browser-homepage',
        iterations: 1,
        metrics: {
            browser_load_ms: 98.765,
            browser_request_count: 4,
            success_rate: 1,
        },
        artifacts: {
            raw_result: { kind: 'json', label: 'Raw result', path: 'raw-result.json' },
            screenshot: { path: 'screenshot.png' },
            trace: { kind: 'playwright-trace', path: 'trace.zip' },
        },
        metadata: {
            browser_evidence_schema: HOMEBOY_BROWSER_EVIDENCE_SCHEMA,
            route: '/wp-admin/',
        },
    });
});
