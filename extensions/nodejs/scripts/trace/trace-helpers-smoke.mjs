import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';

const root = await mkdtemp(path.join(tmpdir(), 'homeboy-node-trace-helpers-'));
const artifactDir = path.join(root, 'artifacts');
const resultsFile = path.join(root, 'trace-results.json');
process.env.HOMEBOY_TRACE_ARTIFACT_DIR = artifactDir;

try {
    const { createTraceReporter } = await import('./lib/timeline.mjs');
    const { createBrowserWaterfallCollector, normalizeBrowserWaterfall } = await import('./lib/browser-waterfall.mjs');

    const artifactPath = path.join(artifactDir, 'metrics.json');
    await mkdir(artifactDir, { recursive: true });
    await writeFile(artifactPath, '{"ok":true}\n');

    const reporter = createTraceReporter({
        componentId: 'fixture-component',
        scenarioId: 'fixture-scenario',
        resultsFile,
    });

    reporter.mark('start', { token: 'secret-token', safe: 'value' });
    reporter.artifact({ label: 'Metrics', path: artifactPath, kind: 'json' });
    reporter.assertion({ id: 'fixture-check', status: 'pass', message: 'Fixture passed.', data: { password: 'secret', visible: true } });
    assert.throws(() => reporter.artifact({ label: 'Missing path' }), /non-empty path/);
    const passEnvelope = await reporter.pass({ request_count: 2, apiKey: 'secret-key' }, { summary: 'Fixture trace passed.' });

    assert.equal(passEnvelope.status, 'pass');
    assert.equal(passEnvelope.component_id, 'fixture-component');
    assert.equal(passEnvelope.artifacts.find((artifact) => artifact.label === 'Metrics')?.path, 'metrics.json');
    assert.equal(passEnvelope.artifacts.find((artifact) => artifact.label === 'timeline')?.path, 'trace.jsonl');
    assert.equal(passEnvelope.timeline[0].data.token, '[Redacted]');
    assert.equal(passEnvelope.assertions[0].data.password, '[Redacted]');
    assert.equal(passEnvelope.metrics.apiKey, '[Redacted]');

    const written = JSON.parse(await readFile(resultsFile, 'utf8'));
    assert.deepEqual(written, passEnvelope);

    const failReporter = createTraceReporter({
        componentId: 'fixture-component',
        scenarioId: 'fixture-fail',
        resultsFile: path.join(root, 'trace-fail.json'),
    });
    const failEnvelope = await failReporter.fail(new Error('boom'), { failed: true });
    assert.equal(failEnvelope.status, 'fail');
    assert.equal(failEnvelope.failure.message, 'boom');

    const snapshot = {
        page_url: 'https://example.test/wp-admin/site-editor.php',
        resources: [
            {
                name: 'https://example.test/wp-json/wp/v2/pages?context=edit',
                initiatorType: 'fetch',
                startTime: 120,
                requestStart: 122,
                responseStart: 150,
                responseEnd: 210,
                duration: 90,
                transferSize: 2048,
                encodedBodySize: 1024,
                decodedBodySize: 4096,
            },
            {
                name: 'https://example.test/wp-content/app.js',
                initiatorType: 'script',
                startTime: 30,
                responseStart: 35,
                responseEnd: 55,
                duration: 25,
                transferSize: 512,
            },
        ],
        phase_marks: [
            { name: 'boot', start_time_ms: 0 },
            { name: 'editor_ready', start_time_ms: 100 },
        ],
    };

    const waterfall = normalizeBrowserWaterfall(snapshot);
    assert.equal(waterfall.schema, 'homeboy/browser-waterfall/v1');
    assert.equal(waterfall.summary.request_count, 2);
    assert.equal(waterfall.summary.transfer_size_bytes, 2560);
    assert.equal(waterfall.rows[0].resource_type, 'script');
    assert.equal(waterfall.rows[1].phase, 'editor_ready');
    assert.equal(waterfall.rows[1].ttfb_ms, 30);

    const fakePage = {
        async evaluate(fn) {
            assert.equal(fn.name, 'browserWaterfallSnapshot');
            return snapshot;
        },
    };
    const collector = createBrowserWaterfallCollector({ page: fakePage });
    const collected = await collector.collect();
    assert.deepEqual(collected, waterfall);

    console.log('nodejs trace helpers smoke passed');
} finally {
    await rm(root, { recursive: true, force: true });
}
