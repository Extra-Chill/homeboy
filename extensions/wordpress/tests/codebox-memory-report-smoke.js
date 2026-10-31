'use strict';

const assert = require('assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { spawnSync } = require('child_process');

const {
	compareCodeboxMemoryResults,
	extractCodeboxMemoryScenarios,
	formatCodeboxMemoryComparisonMarkdown,
	normalizeThresholds,
} = require('../lib/codebox-memory-report');

const baseline = {
	component_id: 'wp-codebox-fixture',
	scenarios: [
		{
			id: 'browser-memory/site-editor',
			label: 'Site editor',
			metadata: { backend: 'wp-codebox' },
			metrics: {
				browser_memory_peak_heap_bytes: 100 * 1024 * 1024,
				browser_memory_post_idle_retained_heap_bytes: 78 * 1024 * 1024,
				browser_memory_long_task_total_ms: 200,
				browser_memory_resource_count: 50,
				browser_memory_transfer_size_bytes: 1024 * 1024,
				browser_memory_checkpoints: [
					{ checkpoint: 'shell ready', heap_bytes: 42 * 1024 * 1024 },
					{ checkpoint: 'preview ready', heap_bytes: 91 * 1024 * 1024 },
					{ checkpoint: 'post idle', heap_bytes: 78 * 1024 * 1024 },
				],
			},
		},
	],
};

const candidate = {
	component_id: 'wp-codebox-fixture',
	scenarios: [
		{
			id: 'browser-memory/site-editor',
			label: 'Site editor',
			metadata: { backend: 'wp-codebox' },
			metrics: {
				browser_memory_peak_heap_bytes: 112 * 1024 * 1024,
				browser_memory_post_idle_retained_heap_bytes: 86 * 1024 * 1024,
				browser_memory_long_task_total_ms: 260,
				browser_memory_resource_count: 54,
				browser_memory_transfer_size_bytes: 1536 * 1024,
				browser_memory_checkpoints: [
					{ checkpoint: 'shell ready', heap_bytes: 40 * 1024 * 1024 },
					{ checkpoint: 'preview ready', heap_bytes: 104 * 1024 * 1024 },
					{ checkpoint: 'post idle', heap_bytes: 86 * 1024 * 1024 },
				],
			},
		},
	],
};

const scenarios = extractCodeboxMemoryScenarios(baseline);
assert.equal(scenarios.length, 1);
assert.equal(scenarios[0].metrics.peakHeapBytes, 100 * 1024 * 1024);
assert.equal(scenarios[0].checkpoints.length, 3);

assert.deepEqual(normalizeThresholds({ fail_peak_heap_percent: 10, warn_resource_count: 3 }), {
	failPeakHeapPercent: 10,
	failPostIdleRetainedHeapBytes: null,
	warnLongTaskTotalPercent: null,
	warnResourceCount: 3,
	warnTransferSizeBytes: null,
});

const reportOnlyComparison = compareCodeboxMemoryResults({ baseline, candidate });
assert.equal(reportOnlyComparison.status, 'reported');
assert.equal(reportOnlyComparison.findings.length, 0);
assert.equal(reportOnlyComparison.scenarios[0].metrics.peakHeapBytes.delta, 12 * 1024 * 1024);
assert.equal(reportOnlyComparison.scenarios[0].checkpoints[1].deltaHeapBytes, 13 * 1024 * 1024);

const gatedComparison = compareCodeboxMemoryResults({
	baseline,
	candidate,
	thresholds: {
		fail_peak_heap_percent: 10,
		fail_post_idle_retained_heap_bytes: 7 * 1024 * 1024,
		warn_long_task_total_percent: 20,
		warn_resource_count: 3,
		warn_transfer_size_bytes: 256 * 1024,
	},
});
assert.equal(gatedComparison.status, 'failed');
assert.equal(gatedComparison.findings.filter((finding) => finding.status === 'fail').length, 2);
assert.equal(gatedComparison.findings.filter((finding) => finding.status === 'warn').length, 3);

const markdown = formatCodeboxMemoryComparisonMarkdown(gatedComparison);
assert.match(markdown, /Codebox Browser Memory Comparison/);
assert.match(markdown, /\| Checkpoint \| Baseline Heap \| Candidate Heap \| Delta \|/);
assert.match(markdown, /\| shell ready \| 42 MB \| 40 MB \| -2 MB \|/);
assert.match(markdown, /\| preview ready \| 91 MB \| 104 MB \| \+13 MB \|/);
assert.match(markdown, /\| Peak heap \| 100 MB \| 112 MB \| \+12 MB \|/);

const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-codebox-memory-report-'));
try {
	const baselinePath = path.join(tmpDir, 'baseline.json');
	const candidatePath = path.join(tmpDir, 'candidate.json');
	fs.writeFileSync(baselinePath, JSON.stringify(baseline), 'utf8');
	fs.writeFileSync(candidatePath, JSON.stringify(candidate), 'utf8');
	const cli = spawnSync(process.execPath, [
		path.join(__dirname, '..', 'lib', 'codebox-memory-report.js'),
		'--baseline',
		baselinePath,
		'--candidate',
		candidatePath,
		'--thresholds-json',
		'{"warn_resource_count":3}',
	], { encoding: 'utf8' });
	assert.equal(cli.status, 0, cli.stderr);
	assert.match(cli.stdout, /Status: \*\*warning\*\*/);
	assert.match(cli.stdout, /Resource count/);
} finally {
	fs.rmSync(tmpDir, { recursive: true, force: true });
}

console.log('Codebox memory report smoke passed');
