import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import test from 'node:test';

import {
    HOMEBOY_BENCH_RESULTS_SCHEMA,
    HOMEBOY_BROWSER_EVIDENCE_SCHEMA,
    buildBenchResultsEnvelope,
    buildBrowserBenchResult,
    normalizeBrowserArtifact,
    normalizeBrowserBottleneck,
    normalizeBrowserPerformanceProfile,
    normalizeBrowserBenchWorkloadResult,
    normalizeBrowserProfileTimings,
    normalizeBrowserTiming,
    normalizeTraceAssertion,
    normalizeTraceEnvelope,
} from '../scripts/lib/browser-result-shapes.mjs';

const require = createRequire(import.meta.url);

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

test('browser result shapes expose equivalent CommonJS helpers', () => {
    const shapes = require('../scripts/lib/browser-result-shapes.cjs');

    const profile = shapes.normalizeBrowserPerformanceProfile({
        page_url: 'https://example.test/app',
        network: [
            {
                url: 'https://example.test/app.js',
                method: 'get',
                status: 200,
                start_time_ms: 1.2345,
                duration_ms: 5.6789,
            },
        ],
        phase_marks: [
            { name: 'load', start_time_ms: 0 },
            { name: 'ready', start_time_ms: 20 },
        ],
        summary: { network_request_count: 1 },
    });
    assert.equal(profile.schema_version, 1);
    assert.equal(profile.network[0].url, 'https://example.test/app.js');
    assert.equal(profile.phases.load.duration_ms, 20);

    const timing = shapes.normalizeBrowserTiming(
        {
            name: 'https://example.test/wp-json/demo?_wpnonce=secret',
            method: 'post',
            startTime: 10,
            responseStart: 30,
            responseEnd: 50,
        },
        {
            normalizeUrl: (url) => new URL(url).pathname,
        }
    );
    assert.equal(timing.normalizedUrl, '/wp-json/demo');
    assert.equal(timing.method, 'POST');
    assert.equal(timing.ttfbMs, 20);
    assert.equal(timing.durationMs, 40);

    const rows = shapes.normalizeBrowserProfileTimings(
        {
            resources: [
                { name: 'https://example.test/app.js', startTime: 5, responseStart: 8, responseEnd: 12 },
            ],
            network: [{ url: 'https://example.test/app.js', method: 'GET', duration_ms: 9 }],
            phases: { load: { start_time_ms: 0, end_time_ms: 10 } },
        },
        {
            normalizeUrl: (url) => new URL(url).pathname,
        }
    );
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
});

test('browser result shapes expose ESM helper surface', () => {
    const artifact = normalizeBrowserArtifact({ path: 'trace.zip', kind: 'playwright-trace', label: 'Trace' });
    assert.deepEqual(artifact, { kind: 'playwright-trace', label: 'Trace', path: 'trace.zip' });

    const bottleneck = normalizeBrowserBottleneck({
        kind: 'network',
        phase: 'load',
        message: 'slow request',
        data: { duration_ms: 25 },
    });
    assert.equal(bottleneck.kind, 'network');
    assert.equal(bottleneck.data.duration_ms, 25);

    assert.equal(typeof normalizeBrowserPerformanceProfile, 'function');
    assert.equal(typeof normalizeBrowserTiming, 'function');
    assert.equal(typeof normalizeBrowserProfileTimings, 'function');
    assert.equal(typeof normalizeTraceAssertion, 'function');
    assert.equal(typeof normalizeTraceEnvelope, 'function');
});
