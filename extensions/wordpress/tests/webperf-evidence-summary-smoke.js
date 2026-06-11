'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const {
	formatWebperfEvidenceSummaryMarkdown,
	summarizeWebperfEvidence,
} = require('../lib/webperf-evidence-summary');

function writeJson(filePath, value) {
	fs.mkdirSync(path.dirname(filePath), { recursive: true });
	fs.writeFileSync(filePath, `${JSON.stringify(value, null, 2)}\n`);
}

function withTempDirectory(callback) {
	const root = fs.mkdtempSync(path.join(os.tmpdir(), 'webperf-evidence-summary-'));
	try {
		callback(root);
	} finally {
		fs.rmSync(root, { recursive: true, force: true });
	}
}

const baseline = {
	component_id: 'fixture-plugin',
	scenarios: [
		{
			id: 'site-editor',
			metrics: {
				ready_ms: 1000,
				rest_request_count: 8,
				success_mean: 1,
			},
		},
		{
			id: 'checkout',
			metrics: { ready_ms: 1200 },
		},
	],
};
const candidate = {
	component_id: 'fixture-plugin',
	artifacts: {
		compare_json: { path: 'compare.json', kind: 'json' },
	},
	scenarios: [
		{
			id: 'site-editor',
			metrics: {
				ready_ms: 850,
				rest_request_count: 5,
				success_mean: 1,
			},
			artifacts: {
				trace: { path: 'traces/site-editor.trace.json', kind: 'json' },
			},
		},
		{
			id: 'checkout',
			metrics: { ready_ms: 1210 },
		},
	],
};

const focused = summarizeWebperfEvidence({ baseline, candidate }, {
	metrics: ['ready_ms', 'rest_request_count'],
	scenarios: ['site-editor'],
});
assert.equal(focused.schema, 'homeboy/webperf-evidence-summary/v1');
assert.equal(focused.verdict, 'improvement');
assert.equal(focused.measurement_rows.length, 2);
assert.equal(focused.measurement_rows.find((row) => row.metric === 'ready_ms').percent_delta, -15);
assert.equal(focused.measurement_rows.find((row) => row.metric === 'rest_request_count').verdict, 'improvement');
assert.equal(focused.artifacts.some((artifact) => artifact.name === 'compare_json'), true);
assert.equal(focused.artifacts.some((artifact) => artifact.scope === 'scenario:site-editor' && artifact.name === 'trace'), true);
assert.equal(focused.caveats.length, 0);
assert.match(formatWebperfEvidenceSummaryMarkdown(focused), /Verdict: \*\*improvement\*\*/);
assert.match(formatWebperfEvidenceSummaryMarkdown(focused), /site-editor/);

const safety = summarizeWebperfEvidence({
	component_id: 'fixture-plugin',
	scenarios: [{ id: 'site-editor', metrics: { ready_ms: 850 } }],
}, { metrics: ['ready_ms', 'missing_metric'] });
assert.equal(safety.verdict, 'safety');
assert.equal(safety.caveats.some((caveat) => caveat.includes('No baseline/candidate metric pair')), true);
assert.equal(safety.caveats.some((caveat) => caveat.includes('missing_metric')), true);

const regression = summarizeWebperfEvidence({ baseline, candidate: {
	...candidate,
	scenarios: [{ id: 'site-editor', metrics: { ready_ms: 1110 } }],
} }, { metrics: ['ready_ms'] });
assert.equal(regression.verdict, 'regression');

withTempDirectory((root) => {
	const baselinePath = path.join(root, 'baseline.json');
	const candidatePath = path.join(root, 'candidate.json');
	const jsonPath = path.join(root, 'summary.json');
	const markdownPath = path.join(root, 'summary.md');
	writeJson(baselinePath, baseline);
	writeJson(candidatePath, candidate);
	const result = spawnSync(process.execPath, [
		path.join(__dirname, '..', 'lib', 'webperf-evidence-summary.js'),
		'--baseline', baselinePath,
		'--candidate', candidatePath,
		'--metrics', JSON.stringify(['ready_ms']),
		'--scenarios', JSON.stringify(['site-editor']),
		'--output-json', jsonPath,
		'--output-markdown', markdownPath,
	], { encoding: 'utf8' });
	assert.equal(result.status, 0, result.stderr);
	assert.equal(JSON.parse(fs.readFileSync(jsonPath, 'utf8')).verdict, 'improvement');
	assert.match(fs.readFileSync(markdownPath, 'utf8'), /Web Performance Evidence Summary/);
});

console.log('✓ Webperf evidence summary smoke test PASSED');
