'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
	collectBenchmarkMatrixCells,
	normalizeBenchmarkMetricRows,
	rankSlowPathCells,
	renderBenchmarkMatrixMarkdownReport,
	summarizeBenchmarkMatrixReport,
} = require('../lib/benchmark-matrix-report');

const fixture = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-benchmark-matrix-'));

try {
	const baselineDir = path.join(fixture, 'baseline');
	const candidateDir = path.join(fixture, 'candidate');
	fs.mkdirSync(baselineDir, { recursive: true });
	fs.mkdirSync(candidateDir, { recursive: true });

	fs.writeFileSync(
		path.join(baselineDir, 'bench-results.json'),
		`${JSON.stringify({
			run_id: 'baseline-run',
			scenarios: [
				{
					id: 'small-catalog',
					label: 'Small catalog',
					metadata: { dimensions: { shape: 'small' } },
					metrics: { duration_ms: 100, queries: 40, items: 10, guardrail_failures: 0 },
				},
				{
					id: 'large-catalog',
					label: 'Large catalog',
					metadata: { dimensions: { shape: 'large' } },
					metrics: { duration_ms: 180, queries: 90, items: 30, guardrail_failures: 0 },
				},
			],
		}, null, 2)}\n`,
		'utf8'
	);

	fs.mkdirSync(path.join(candidateDir, 'child-a'), { recursive: true });
	fs.writeFileSync(
		path.join(candidateDir, 'child-a', 'bench-results.json'),
		`${JSON.stringify({
			run_id: 'candidate-run',
			run_url: 'https://example.test/runs/candidate-run',
			artifacts: { bundle: 'bundle.json' },
			scenarios: [
				{
					id: 'small-catalog',
					label: 'Small catalog',
					metadata: { dimensions: { shape: 'small' } },
					metrics: { duration_ms: 120, queries: 60, items: 10, guardrail_failures: 1 },
				},
				{
					id: 'large-catalog',
					label: 'Large catalog',
					metadata: { dimensions: { shape: 'large' } },
					metrics: { duration_ms: 210, queries: 150, items: 30, guardrail_failures: 0 },
				},
			],
		}, null, 2)}\n`,
		'utf8'
	);

	const baselineCells = collectBenchmarkMatrixCells(baselineDir);
	const candidateCells = collectBenchmarkMatrixCells(candidateDir);
	assert.equal(baselineCells.length, 2);
	assert.equal(candidateCells.length, 2);
	assert.equal(candidateCells[0].artifact_path, path.join('child-a', 'bench-results.json'));

	const metricRows = normalizeBenchmarkMetricRows(candidateCells, {
		baselineCells,
		metrics: [
			{ name: 'queries', label: 'Queries', denominator: 'items', perUnitLabel: 'Queries/item', threshold: { warn: 4, fail: 6 } },
			{ name: 'duration_ms', label: 'Duration', denominator: 'items', perUnitLabel: 'Duration/item' },
			{ name: 'guardrail_failures', label: 'Guardrail failures', threshold: { fail: 1 } },
		],
	});
	assert.equal(metricRows.length, 6);

	const smallQueries = metricRows.find((row) => row.scenario_id === 'small-catalog' && row.metric === 'queries');
	assert.equal(smallQueries.per_unit_value, 6);
	assert.equal(smallQueries.baseline_per_unit_value, 4);
	assert.equal(smallQueries.percent_delta, 50);

	const ranked = rankSlowPathCells(metricRows, { rankMetrics: ['queries', 'duration_ms'] });
	assert.equal(ranked.length, 2);
	assert.equal(ranked[0].scenario_id, 'small-catalog');
	assert.equal(ranked[0].status, 'fail');
	assert.equal(ranked[1].scenario_id, 'large-catalog');
	assert.equal(ranked[1].status, 'warn');

	const summary = summarizeBenchmarkMatrixReport(metricRows, { rankMetrics: ['queries', 'duration_ms'] });
	assert.equal(summary.schema, 'homeboy/wordpress-benchmark-slow-path-matrix/v1');
	assert.equal(summary.ranked_cells[0].run_url, 'https://example.test/runs/candidate-run');

	const markdown = renderBenchmarkMatrixMarkdownReport(summary);
	assert.match(markdown, /# WordPress Benchmark Slow-Path Matrix/);
	assert.match(markdown, /Small catalog/);
	assert.match(markdown, /Queries\/item: 6/);
	assert.match(markdown, /\[candidate-run\]\(https:\/\/example\.test\/runs\/candidate-run\)/);

	console.log('WordPress benchmark matrix report smoke passed.');
} finally {
	fs.rmSync(fixture, { recursive: true, force: true });
}
