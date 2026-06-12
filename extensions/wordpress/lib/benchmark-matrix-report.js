'use strict';

/**
 * External dependencies
 */
const fs = require('node:fs');
const path = require('node:path');

function isPlainObject(value) {
	return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function asArray(value) {
	if (Array.isArray(value)) {
		return value;
	}
	if (typeof value === 'string' && value.trim() !== '') {
		return value.split(',').map((item) => item.trim()).filter(Boolean);
	}
	return [];
}

function finiteNumber(value) {
	if (typeof value === 'number' && Number.isFinite(value)) {
		return value;
	}
	if (typeof value === 'string' && value.trim() !== '') {
		const number = Number(value);
		return Number.isFinite(number) ? number : null;
	}
	return null;
}

function round(value, places = 3) {
	const number = finiteNumber(value);
	if (number === null) {
		return null;
	}
	const factor = 10 ** places;
	return Math.round(number * factor) / factor;
}

function readJsonFile(filePath) {
	return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

function collectJsonFiles(root) {
	const resolved = path.resolve(root);
	const stats = fs.statSync(resolved);
	if (stats.isFile()) {
		return resolved.endsWith('.json') ? [resolved] : [];
	}

	const files = [];
	const visit = (directory) => {
		for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
			const entryPath = path.join(directory, entry.name);
			if (entry.isDirectory()) {
				visit(entryPath);
			} else if (entry.isFile() && entry.name.endsWith('.json')) {
				files.push(entryPath);
			}
		}
	};
	visit(resolved);
	return files.sort();
}

function metricValue(source, name) {
	if (!name) {
		return null;
	}
	return finiteNumber(source?.metrics?.[name])
		?? finiteNumber(source?.metadata?.[name])
		?? finiteNumber(source?.[name]);
}

function objectValue(source, keys) {
	for (const key of asArray(keys)) {
		const value = source?.[key] ?? source?.metadata?.[key] ?? source?.metrics?.[key];
		if (value !== undefined && value !== null && value !== '') {
			return value;
		}
	}
	return null;
}

function extractArtifactReferences(source) {
	const artifacts = [];
	const addArtifacts = (scope, value) => {
		if (!isPlainObject(value)) {
			return;
		}
		for (const [name, artifact] of Object.entries(value)) {
			if (typeof artifact === 'string') {
				artifacts.push({ scope, name, path: artifact });
			} else if (isPlainObject(artifact)) {
				artifacts.push({ scope, name, ...artifact });
			}
		}
	};
	addArtifacts('run', source?.artifacts);
	addArtifacts('metadata', source?.metadata?.artifacts);
	return artifacts;
}

function stableJson(value) {
	return JSON.stringify(value || {});
}

function cellKey(cell) {
	return stableJson({ scenario_id: cell.scenario_id, dimensions: cell.dimensions });
}

function normalizeDimensions(source, options = {}) {
	const dimensions = {};
	for (const key of asArray(options.dimensionKeys)) {
		const value = objectValue(source, [key]);
		if (value !== null) {
			dimensions[key] = value;
		}
	}
	if (isPlainObject(source?.dimensions)) {
		Object.assign(dimensions, source.dimensions);
	}
	if (isPlainObject(source?.metadata?.dimensions)) {
		Object.assign(dimensions, source.metadata.dimensions);
	}
	return dimensions;
}

function normalizeCell(source, context = {}) {
	const scenarioId = source?.scenario_id || source?.id || source?.name || context.runId || 'unknown';
	return {
		cell_id: source?.cell_id || source?.id || `${scenarioId}:${context.index ?? 0}`,
		scenario_id: String(scenarioId),
		label: source?.label || source?.title || source?.metadata?.label || String(scenarioId),
		run_id: source?.run_id || source?.metadata?.run_id || context.runId || null,
		run_url: source?.run_url || source?.metadata?.run_url || context.runUrl || null,
		artifact_path: context.artifactPath || null,
		metrics: isPlainObject(source?.metrics) ? source.metrics : {},
		metadata: isPlainObject(source?.metadata) ? source.metadata : {},
		dimensions: normalizeDimensions(source, context.options),
		artifacts: extractArtifactReferences(source),
	};
}

function extractCellsFromJson(payload, context = {}) {
	if (Array.isArray(payload?.cells)) {
		return payload.cells.map((cell, index) => normalizeCell(cell, { ...context, index }));
	}
	if (Array.isArray(payload?.scenarios)) {
		return payload.scenarios
			.filter((scenario) => (scenario?.id || scenario?.scenario_id) !== '__bootstrap')
			.map((scenario, index) => normalizeCell(scenario, {
				...context,
				index,
				runId: payload.run_id || payload.metadata?.run_id || context.runId,
				runUrl: payload.run_url || payload.metadata?.run_url || context.runUrl,
			}));
	}
	if (isPlainObject(payload?.metrics) || isPlainObject(payload?.metadata)) {
		return [normalizeCell(payload, context)];
	}
	return [];
}

function collectBenchmarkMatrixCells(artifactRoot, options = {}) {
	if (typeof artifactRoot !== 'string' || artifactRoot.trim() === '') {
		throw new TypeError('artifactRoot must be a non-empty path string');
	}
	const root = path.resolve(artifactRoot);
	const cells = [];
	for (const filePath of collectJsonFiles(root)) {
		let payload;
		try {
			payload = readJsonFile(filePath);
		} catch (error) {
			if (options.ignoreInvalidJson) {
				continue;
			}
			throw new Error(`Invalid benchmark matrix artifact JSON at ${filePath}: ${error.message}`);
		}
		cells.push(...extractCellsFromJson(payload, {
			artifactPath: path.relative(root, filePath) || path.basename(filePath),
			options,
		}));
	}
	return cells;
}

function normalizeMetricSpec(spec) {
	if (typeof spec === 'string') {
		return { name: spec, label: spec };
	}
	if (!isPlainObject(spec) || typeof spec.name !== 'string' || spec.name.trim() === '') {
		throw new TypeError('metric specs must be strings or objects with a name');
	}
	return {
		...spec,
		label: spec.label || spec.name,
	};
}

function inferMetricSpecs(cells, options = {}) {
	const explicit = asArray(options.metrics).map(normalizeMetricSpec);
	if (explicit.length > 0) {
		return explicit;
	}
	const names = new Set();
	for (const cell of cells) {
		for (const [name, value] of Object.entries(cell.metrics || {})) {
			if (finiteNumber(value) !== null) {
				names.add(name);
			}
		}
	}
	return [...names].sort().map((name) => ({ name, label: name }));
}

function normalizeMetricRowsForCells(cells, options = {}) {
	const metricSpecs = inferMetricSpecs(cells, options);
	const baselineByKey = new Map(asArray(options.baselineCells).map((cell) => [cellKey(cell), cell]));
	const rows = [];
	for (const cell of cells) {
		const baselineCell = baselineByKey.get(cellKey(cell));
		for (const metric of metricSpecs) {
			const value = metricValue(cell, metric.name);
			const denominatorName = metric.denominator || options.denominators?.[metric.name] || options.defaultDenominator;
			const denominator = metricValue(cell, denominatorName);
			const perUnitValue = value !== null && denominator !== null && denominator !== 0 ? value / denominator : null;
			const baselineValue = baselineCell ? metricValue(baselineCell, metric.name) : null;
			const baselineDenominator = baselineCell && denominatorName ? metricValue(baselineCell, denominatorName) : null;
			const baselinePerUnitValue = baselineValue !== null && baselineDenominator !== null && baselineDenominator !== 0
				? baselineValue / baselineDenominator
				: null;
			if (value === null && baselineValue === null) {
				continue;
			}
			const rankValue = perUnitValue ?? value ?? 0;
			const baselineRankValue = baselinePerUnitValue ?? baselineValue;
			rows.push({
				cell_id: cell.cell_id,
				scenario_id: cell.scenario_id,
				label: cell.label,
				dimensions: cell.dimensions,
				run_id: cell.run_id,
				run_url: cell.run_url,
				artifact_path: cell.artifact_path,
				artifacts: cell.artifacts,
				metric: metric.name,
				metric_label: metric.label,
				value: round(value),
				denominator: denominatorName || null,
				denominator_value: round(denominator),
				per_unit_value: round(perUnitValue),
				per_unit_label: metric.perUnitLabel || (denominatorName ? `${metric.label}/${denominatorName}` : metric.label),
				baseline_value: round(baselineValue),
				baseline_per_unit_value: round(baselinePerUnitValue),
				delta: round(rankValue - (baselineRankValue ?? rankValue)),
				percent_delta: baselineRankValue && baselineRankValue !== 0 ? round(((rankValue - baselineRankValue) / Math.abs(baselineRankValue)) * 100) : null,
				direction: metric.direction || 'higher-is-slower',
				rank_value: round(rankValue),
				threshold: metric.threshold ?? options.thresholds?.[metric.name] ?? null,
			});
		}
	}
	return rows;
}

function normalizeBenchmarkMetricRows(cells, options = {}) {
	if (!Array.isArray(cells)) {
		throw new TypeError('cells must be an array');
	}
	return normalizeMetricRowsForCells(cells, options);
}

function thresholdStatus(row) {
	const threshold = row.threshold;
	if (!isPlainObject(threshold)) {
		return 'ok';
	}
	const value = row.rank_value;
	if (value === null) {
		return 'ok';
	}
	if (finiteNumber(threshold.fail) !== null && value >= threshold.fail) {
		return 'fail';
	}
	if (finiteNumber(threshold.warn) !== null && value >= threshold.warn) {
		return 'warn';
	}
	return 'ok';
}

function compareRows(a, b, rankMetrics) {
	const severity = (status) => {
		if (status === 'fail') {
			return 2;
		}
		if (status === 'warn') {
			return 1;
		}
		return 0;
	};
	const aSeverity = severity(a.status);
	const bSeverity = severity(b.status);
	if (aSeverity !== bSeverity) {
		return bSeverity - aSeverity;
	}
	for (const metric of rankMetrics) {
		const delta = (b.metric_values[metric] ?? -Infinity) - (a.metric_values[metric] ?? -Infinity);
		if (delta !== 0) {
			return delta;
		}
	}
	return a.scenario_id.localeCompare(b.scenario_id) || stableJson(a.dimensions).localeCompare(stableJson(b.dimensions));
}

function rankSlowPathCells(rows, options = {}) {
	if (!Array.isArray(rows)) {
		throw new TypeError('rows must be an array');
	}
	const rankMetrics = asArray(options.rankMetrics || options.rankBy);
	const effectiveRankMetrics = rankMetrics.length > 0 ? rankMetrics : [...new Set(rows.map((row) => row.metric))];
	const byCell = new Map();
	for (const row of rows) {
		const key = stableJson({ scenario_id: row.scenario_id, dimensions: row.dimensions });
		if (!byCell.has(key)) {
			byCell.set(key, {
				cell_id: row.cell_id,
				scenario_id: row.scenario_id,
				label: row.label,
				dimensions: row.dimensions || {},
				run_id: row.run_id,
				run_url: row.run_url,
				artifact_path: row.artifact_path,
				artifacts: row.artifacts || [],
				metric_values: {},
				metrics: [],
				status: 'ok',
			});
		}
		const cell = byCell.get(key);
		const status = thresholdStatus(row);
		cell.metrics.push({ ...row, status });
		cell.metric_values[row.metric] = row.rank_value;
		if (status === 'fail' || (status === 'warn' && cell.status !== 'fail')) {
			cell.status = status;
		}
	}
	return [...byCell.values()]
		.sort((a, b) => compareRows(a, b, effectiveRankMetrics))
		.slice(0, Number.isFinite(options.limit) ? options.limit : undefined)
		.map((cell, index) => ({ ...cell, rank: index + 1 }));
}

function summarizeBenchmarkMatrixReport(rows, options = {}) {
	const rankedCells = rankSlowPathCells(rows, options);
	return {
		schema: 'homeboy/wordpress-benchmark-slow-path-matrix/v1',
		generated_from: 'benchmark-matrix-artifacts',
		rank_metrics: asArray(options.rankMetrics || options.rankBy),
		row_count: rows.length,
		cell_count: rankedCells.length,
		ranked_cells: rankedCells,
	};
}

function formatValue(value, suffix = '') {
	return value === null || value === undefined ? 'n/a' : `${value}${suffix}`;
}

function formatDimensions(dimensions) {
	const entries = Object.entries(dimensions || {});
	if (entries.length === 0) {
		return '';
	}
	return entries.map(([key, value]) => `${key}=${value}`).join(', ');
}

function markdownEscape(value) {
	return String(value ?? '').replace(/\|/g, '\\|').replace(/\n/g, ' ');
}

function renderBenchmarkMatrixMarkdownReport(summaryOrRows, options = {}) {
	const summary = Array.isArray(summaryOrRows) ? summarizeBenchmarkMatrixReport(summaryOrRows, options) : summaryOrRows;
	const lines = [
		'# WordPress Benchmark Slow-Path Matrix',
		'',
		`Cells ranked: **${summary.cell_count}**`,
		'',
		'| Rank | Status | Scenario | Dimensions | Top metrics | Run | Artifact |',
		'|---:|---|---|---|---|---|---|',
	];
	for (const cell of summary.ranked_cells) {
		const metrics = cell.metrics
			.slice()
			.sort((a, b) => (b.rank_value ?? -Infinity) - (a.rank_value ?? -Infinity))
			.slice(0, options.metricLimit || 4)
			.map((row) => `${row.per_unit_value === null ? row.metric_label : row.per_unit_label}: ${formatValue(row.per_unit_value ?? row.value)}${row.percent_delta === null ? '' : ` (${formatValue(row.percent_delta, '%')})`}`)
			.join('<br>');
		const run = cell.run_url ? `[${cell.run_id || 'run'}](${cell.run_url})` : (cell.run_id || '');
		lines.push(`| ${cell.rank} | ${cell.status} | ${markdownEscape(cell.label || cell.scenario_id)} | ${markdownEscape(formatDimensions(cell.dimensions))} | ${markdownEscape(metrics)} | ${run} | ${markdownEscape(cell.artifact_path || '')} |`);
	}
	if (summary.ranked_cells.length === 0) {
		lines.push('| n/a | ok | n/a | n/a | n/a | n/a | n/a |');
	}
	return `${lines.join('\n')}\n`;
}

function parseCli(argv) {
	const args = {};
	for (let index = 0; index < argv.length; index += 1) {
		const arg = argv[index];
		if (!arg.startsWith('--')) {
			continue;
		}
		const key = arg.slice(2);
		const next = argv[index + 1];
		if (!next || next.startsWith('--')) {
			args[key] = true;
			continue;
		}
		args[key] = next;
		index += 1;
	}
	return args;
}

function parseJsonOption(value, fallback) {
	return value ? JSON.parse(value) : fallback;
}

function runCli(argv = process.argv.slice(2)) {
	const args = parseCli(argv);
	if (!args.input && !args.candidate) {
		throw new Error('Usage: node benchmark-matrix-report.js --input <artifact-root> [--baseline <artifact-root>] [--output-json <file>] [--output-markdown <file>]');
	}
	const options = {
		metrics: parseJsonOption(args.metrics, undefined),
		thresholds: parseJsonOption(args.thresholds, undefined),
		denominators: parseJsonOption(args.denominators, undefined),
		dimensionKeys: parseJsonOption(args.dimensions, undefined),
		rankMetrics: parseJsonOption(args['rank-metrics'], undefined),
		defaultDenominator: args['default-denominator'],
	};
	const baselineCells = args.baseline ? collectBenchmarkMatrixCells(args.baseline, options) : [];
	const candidateCells = collectBenchmarkMatrixCells(args.candidate || args.input, options);
	const rows = normalizeBenchmarkMetricRows(candidateCells, { ...options, baselineCells });
	const summary = summarizeBenchmarkMatrixReport(rows, options);
	const markdown = renderBenchmarkMatrixMarkdownReport(summary, options);
	if (args['output-json']) {
		fs.writeFileSync(args['output-json'], `${JSON.stringify(summary, null, 2)}\n`);
	}
	if (args['output-markdown']) {
		fs.writeFileSync(args['output-markdown'], markdown);
	} else {
		process.stdout.write(markdown);
	}
	return 0;
}

if (require.main === module) {
	try {
		process.exitCode = runCli();
	} catch (error) {
		process.stderr.write(`${error.message}\n`);
		process.exitCode = 2;
	}
}

module.exports = {
	collectBenchmarkMatrixCells,
	normalizeBenchmarkMetricRows,
	rankSlowPathCells,
	renderBenchmarkMatrixMarkdownReport,
	summarizeBenchmarkMatrixReport,
};
