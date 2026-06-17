import assert from 'node:assert/strict';
import test from 'node:test';

import {
    HOMEBOY_BENCH_RESULTS_SCHEMA,
    HOMEBOY_BROWSER_EVIDENCE_SCHEMA,
    buildBenchResultsEnvelope,
    buildBrowserBenchResult,
    normalizeBrowserBenchWorkloadResult,
} from '../scripts/lib/browser-result-shapes.mjs';

test('buildBenchResultsEnvelope emits Homeboy core schema shape', () => {
    const envelope = buildBenchResultsEnvelope({
        componentId: 'example-component',
        iterations: 2,
        scenarios: [
            {
                id: 'browser-homepage',
                metrics: { p95_ms: 123.4567, ignored: Number.NaN },
                artifacts: { report: { path: 'report.md', kind: 'markdown', extra: 'dropped' } },
            },
        ],
    });

    assert.equal(envelope.schema, HOMEBOY_BENCH_RESULTS_SCHEMA);
    assert.equal(envelope.component_id, 'example-component');
    assert.equal(envelope.iterations, 2);
    assert.deepEqual(envelope.scenarios[0], {
        id: 'browser-homepage',
        iterations: 1,
        metrics: { p95_ms: 123.457 },
        artifacts: { report: { kind: 'markdown', path: 'report.md' } },
    });
});

test('buildBrowserBenchResult composes browser evidence workload result', () => {
    const result = buildBrowserBenchResult({
        browserResult: {
            metrics: { browser_request_count: 4, browser_load_ms: 98.7654 },
            artifacts: { trace: { path: 'trace.zip', kind: 'playwright-trace' } },
        },
        metrics: { success_rate: 1, skipped: Infinity },
        rawResultArtifact: { path: 'raw-result.json', kind: 'json', label: 'Raw result' },
        artifacts: { screenshot: 'screenshot.png' },
        metadata: { route: '/wp-admin/' },
    });

    assert.deepEqual(result.metrics, {
        browser_load_ms: 98.765,
        browser_request_count: 4,
        success_rate: 1,
    });
    assert.deepEqual(result.artifacts, {
        raw_result: { kind: 'json', label: 'Raw result', path: 'raw-result.json' },
        screenshot: { path: 'screenshot.png' },
        trace: { kind: 'playwright-trace', path: 'trace.zip' },
    });
    assert.deepEqual(result.metadata, {
        browser_evidence_schema: HOMEBOY_BROWSER_EVIDENCE_SCHEMA,
        route: '/wp-admin/',
    });
});

test('normalizeBrowserBenchWorkloadResult only normalizes browser-shaped workloads', () => {
    const standard = { metrics: { custom_metric: 1 }, metadata: { kind: 'standard' } };
    assert.equal(normalizeBrowserBenchWorkloadResult(standard), standard);

    const browser = normalizeBrowserBenchWorkloadResult({
        browserMetrics: { browser_ready_ms: 12.3456 },
        browserArtifacts: { trace: { path: 'trace.zip', kind: 'playwright-trace', ignored: true } },
        metadata: { route: '/' },
    });

    assert.deepEqual(browser, {
        metrics: { browser_ready_ms: 12.346 },
        artifacts: { trace: { kind: 'playwright-trace', path: 'trace.zip' } },
        metadata: {
            browser_evidence_schema: HOMEBOY_BROWSER_EVIDENCE_SCHEMA,
            route: '/',
        },
    });
});
