import assert from 'node:assert/strict';
import test from 'node:test';

import {
    HOMEBOY_BENCH_RESULTS_SCHEMA,
    HOMEBOY_BROWSER_EVIDENCE_SCHEMA,
    buildBenchResultsEnvelope,
    buildBrowserBenchResult,
    normalizeTraceAssertion,
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

test('trace assertions carry details in the field Homeboy core reads', () => {
    // Core's TraceAssertion accepts id, status, message and details, and rejects any other field.
    assert.deepEqual(normalizeTraceAssertion('port-moved', 'pass', 'Moved off a taken port', { port: 8026 }), {
        details: { port: 8026 },
        id: 'port-moved',
        message: 'Moved off a taken port',
        status: 'pass',
    });
});

test('trace recorder writes events and assertions in the shape Homeboy core parses', async () => {
    const { mkdtemp, readFile } = await import('node:fs/promises');
    const { tmpdir } = await import('node:os');
    const { join } = await import('node:path');
    const { createTraceRecorder } = await import('../nodejs/scripts/trace/lib/timeline.mjs');
    const dir = await mkdtemp(join(tmpdir(), 'trace-shape-'));
    const recorder = createTraceRecorder({
        scenarioId: 'shape',
        resultsFile: join(dir, 'results.json'),
        timelinePath: join(dir, 'trace.jsonl'),
    });

    await new Promise((resolve) => setTimeout(resolve, 3));
    await recorder.recordEvent('scenario', 'ready', { port: 8026 });
    recorder.recordCheck('port-moved', true, 'Moved off a taken port', { port: 8026 });
    await recorder.writeTraceResults({ summary: 'ok' });

    const results = JSON.parse(await readFile(join(dir, 'results.json'), 'utf8'));
    // Core reads t_ms as an unsigned integer and assertion extras as `details`.
    assert.ok(Number.isInteger(results.timeline[0].t_ms), `t_ms ${results.timeline[0].t_ms}`);
    assert.deepEqual(results.assertions[0].details, { port: 8026 });
});
