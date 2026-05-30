'use strict';

/**
 * Codebox-specific browser memory comparison helpers for Homeboy bench output.
 */

/**
 * External dependencies
 */
const fs = require('fs');

const DEFAULT_THRESHOLDS = Object.freeze({
	failPeakHeapPercent: null,
	failPostIdleRetainedHeapBytes: null,
	warnLongTaskTotalPercent: null,
	warnResourceCount: null,
	warnTransferSizeBytes: null,
});

const METRIC_ALIASES = Object.freeze({
	peakHeapBytes: [
		'browser_memory_peak_heap_bytes',
		'peak_heap_bytes',
		'peakHeapBytes',
		'peak_js_heap_bytes',
		'js_heap_peak_bytes',
	],
	postIdleRetainedHeapBytes: [
		'browser_memory_post_idle_retained_heap_bytes',
		'post_idle_retained_heap_bytes',
		'post_idle_heap_bytes',
		'postIdleRetainedHeapBytes',
		'postIdleHeapBytes',
	],
	longTaskTotalMs: [
		'browser_memory_long_task_total_ms',
		'long_task_total_ms',
		'longTaskTotalMs',
	],
	resourceCount: [
		'browser_memory_resource_count',
		'resource_count',
		'resourceCount',
	],
	transferSizeBytes: [
		'browser_memory_transfer_size_bytes',
		'resource_transfer_size_bytes',
		'transfer_size_bytes',
		'transferSizeBytes',
	],
});

const CHECKPOINT_ALIASES = [
	'browser_memory_checkpoints',
	'memory_checkpoints',
	'checkpoints',
];

function isPlainObject(value) {
	return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function finiteNumber(value) {
	if (typeof value === 'number' && Number.isFinite(value)) {
		return value;
	}
	if (typeof value === 'string' && value.trim() !== '') {
		const parsed = Number(value);
		return Number.isFinite(parsed) ? parsed : null;
	}
	return null;
}

function firstNumber(source, keys) {
	if (!isPlainObject(source)) {
		return null;
	}
	for (const key of keys) {
		const value = finiteNumber(source[key]);
		if (value !== null) {
			return value;
		}
	}
	return null;
}

function normalizeThresholds(input = {}) {
	const thresholds = { ...DEFAULT_THRESHOLDS };
	if (!isPlainObject(input)) {
		return thresholds;
	}
	const aliases = {
		failPeakHeapPercent: ['failPeakHeapPercent', 'fail_peak_heap_percent', 'peak_heap_percent'],
		failPostIdleRetainedHeapBytes: ['failPostIdleRetainedHeapBytes', 'fail_post_idle_retained_heap_bytes', 'post_idle_retained_heap_bytes'],
		warnLongTaskTotalPercent: ['warnLongTaskTotalPercent', 'warn_long_task_total_percent', 'long_task_total_percent'],
		warnResourceCount: ['warnResourceCount', 'warn_resource_count', 'resource_count'],
		warnTransferSizeBytes: ['warnTransferSizeBytes', 'warn_transfer_size_bytes', 'transfer_size_bytes'],
	};
	for (const [target, keys] of Object.entries(aliases)) {
		const value = firstNumber(input, keys);
		if (value !== null) {
			thresholds[target] = value;
		}
	}
	return thresholds;
}

function scenarioIsCodeboxBrowserMemory(scenario) {
	const metadata = scenario?.metadata || {};
	const metrics = scenario?.metrics || {};
	const kind = String(metadata.kind || metadata.artifact_kind || metadata.profile_kind || '').toLowerCase();
	return (
		kind.includes('browser-memory') ||
		kind.includes('browser_memory') ||
		String(metadata.backend || '').toLowerCase().includes('codebox') ||
		Object.values(METRIC_ALIASES).some((aliases) => aliases.some((key) => Object.prototype.hasOwnProperty.call(metrics, key))) ||
		CHECKPOINT_ALIASES.some((key) => Object.prototype.hasOwnProperty.call(metrics, key))
	);
}

function checkpointLabel(row) {
	return String(row?.checkpoint || row?.name || row?.label || row?.id || '').trim();
}

function normalizeCheckpoints(metrics = {}) {
	let rows = [];
	for (const key of CHECKPOINT_ALIASES) {
		if (Array.isArray(metrics[key])) {
			rows = metrics[key];
			break;
		}
	}
	return rows.map((row) => {
		const label = checkpointLabel(row);
		return {
			checkpoint: label,
			heapBytes: firstNumber(row, ['heap_bytes', 'heapBytes', 'used_heap_bytes', 'usedHeapBytes', 'js_heap_bytes', 'value']),
		};
	}).filter((row) => row.checkpoint && row.heapBytes !== null);
}

function normalizeMemoryScenario(scenario) {
	const metrics = scenario?.metrics || {};
	const normalized = {
		id: String(scenario?.id || scenario?.scenario_id || 'unknown'),
		label: String(scenario?.label || scenario?.id || scenario?.scenario_id || 'unknown'),
		metrics: {
			peakHeapBytes: firstNumber(metrics, METRIC_ALIASES.peakHeapBytes),
			postIdleRetainedHeapBytes: firstNumber(metrics, METRIC_ALIASES.postIdleRetainedHeapBytes),
			longTaskTotalMs: firstNumber(metrics, METRIC_ALIASES.longTaskTotalMs),
			resourceCount: firstNumber(metrics, METRIC_ALIASES.resourceCount),
			transferSizeBytes: firstNumber(metrics, METRIC_ALIASES.transferSizeBytes),
		},
		checkpoints: normalizeCheckpoints(metrics),
	};
	return normalized;
}

function extractCodeboxMemoryScenarios(results) {
	return (Array.isArray(results?.scenarios) ? results.scenarios : [])
		.filter(scenarioIsCodeboxBrowserMemory)
		.map(normalizeMemoryScenario)
		.filter((scenario) => Object.values(scenario.metrics).some((value) => value !== null) || scenario.checkpoints.length > 0);
}

function delta(baseline, candidate) {
	return baseline === null || candidate === null ? null : candidate - baseline;
}

function percentDelta(baseline, candidate) {
	if (baseline === null || candidate === null || baseline === 0) {
		return null;
	}
	return ((candidate - baseline) / baseline) * 100;
}

function evaluateThresholds(metrics, thresholds) {
	const findings = [];
	const peakPercent = percentDelta(metrics.peakHeapBytes.baseline, metrics.peakHeapBytes.candidate);
	if (thresholds.failPeakHeapPercent !== null && peakPercent !== null && peakPercent > thresholds.failPeakHeapPercent) {
		findings.push({ status: 'fail', metric: 'Peak heap', delta: peakPercent, threshold: thresholds.failPeakHeapPercent, unit: '%' });
	}
	const postIdleDelta = metrics.postIdleRetainedHeapBytes.delta;
	if (thresholds.failPostIdleRetainedHeapBytes !== null && postIdleDelta !== null && postIdleDelta > thresholds.failPostIdleRetainedHeapBytes) {
		findings.push({ status: 'fail', metric: 'Post-idle retained heap', delta: postIdleDelta, threshold: thresholds.failPostIdleRetainedHeapBytes, unit: 'bytes' });
	}
	const longTaskPercent = percentDelta(metrics.longTaskTotalMs.baseline, metrics.longTaskTotalMs.candidate);
	if (thresholds.warnLongTaskTotalPercent !== null && longTaskPercent !== null && longTaskPercent > thresholds.warnLongTaskTotalPercent) {
		findings.push({ status: 'warn', metric: 'Long task total', delta: longTaskPercent, threshold: thresholds.warnLongTaskTotalPercent, unit: '%' });
	}
	if (thresholds.warnResourceCount !== null && metrics.resourceCount.delta !== null && metrics.resourceCount.delta > thresholds.warnResourceCount) {
		findings.push({ status: 'warn', metric: 'Resource count', delta: metrics.resourceCount.delta, threshold: thresholds.warnResourceCount, unit: 'resources' });
	}
	if (thresholds.warnTransferSizeBytes !== null && metrics.transferSizeBytes.delta !== null && metrics.transferSizeBytes.delta > thresholds.warnTransferSizeBytes) {
		findings.push({ status: 'warn', metric: 'Transfer size', delta: metrics.transferSizeBytes.delta, threshold: thresholds.warnTransferSizeBytes, unit: 'bytes' });
	}
	return findings;
}

function compareMetric(baseline, candidate, key) {
	return {
		baseline: baseline.metrics[key],
		candidate: candidate.metrics[key],
		delta: delta(baseline.metrics[key], candidate.metrics[key]),
		percentDelta: percentDelta(baseline.metrics[key], candidate.metrics[key]),
	};
}

function compareCheckpoints(baseline, candidate) {
	const candidateByCheckpoint = new Map(candidate.checkpoints.map((row) => [row.checkpoint, row]));
	return baseline.checkpoints.map((baselineRow) => {
		const candidateRow = candidateByCheckpoint.get(baselineRow.checkpoint);
		return {
			checkpoint: baselineRow.checkpoint,
			baselineHeapBytes: baselineRow.heapBytes,
			candidateHeapBytes: candidateRow?.heapBytes ?? null,
			deltaHeapBytes: delta(baselineRow.heapBytes, candidateRow?.heapBytes ?? null),
		};
	}).filter((row) => row.candidateHeapBytes !== null);
}

function compareCodeboxMemoryResults({ baseline, candidate, thresholds = {} }) {
	const normalizedThresholds = normalizeThresholds(thresholds);
	const baselineScenarios = extractCodeboxMemoryScenarios(baseline);
	const candidateScenarios = extractCodeboxMemoryScenarios(candidate);
	const candidateById = new Map(candidateScenarios.map((scenario) => [scenario.id, scenario]));
	const scenarios = baselineScenarios.map((baselineScenario) => {
		const candidateScenario = candidateById.get(baselineScenario.id);
		if (!candidateScenario) {
			return null;
		}
		const metrics = {
			peakHeapBytes: compareMetric(baselineScenario, candidateScenario, 'peakHeapBytes'),
			postIdleRetainedHeapBytes: compareMetric(baselineScenario, candidateScenario, 'postIdleRetainedHeapBytes'),
			longTaskTotalMs: compareMetric(baselineScenario, candidateScenario, 'longTaskTotalMs'),
			resourceCount: compareMetric(baselineScenario, candidateScenario, 'resourceCount'),
			transferSizeBytes: compareMetric(baselineScenario, candidateScenario, 'transferSizeBytes'),
		};
		return {
			id: baselineScenario.id,
			label: baselineScenario.label,
			metrics,
			checkpoints: compareCheckpoints(baselineScenario, candidateScenario),
			findings: evaluateThresholds(metrics, normalizedThresholds),
		};
	}).filter(Boolean);
	const findings = scenarios.flatMap((scenario) => scenario.findings.map((finding) => ({ ...finding, scenario: scenario.id })));
	let status = 'reported';
	if (findings.some((finding) => finding.status === 'fail')) {
		status = 'failed';
	} else if (findings.some((finding) => finding.status === 'warn')) {
		status = 'warning';
	}
	return {
		thresholds: normalizedThresholds,
		status,
		scenarios,
		findings,
	};
}

function formatBytes(value) {
	if (value === null || value === undefined) {
		return 'n/a';
	}
	const abs = Math.abs(value);
	const mb = value / 1024 / 1024;
	if (abs >= 1024 * 1024) {
		return `${Math.round(mb)} MB`;
	}
	if (abs >= 1024) {
		return `${Math.round(value / 1024)} KB`;
	}
	return `${Math.round(value)} B`;
}

function formatNumber(value, suffix = '') {
	if (value === null || value === undefined) {
		return 'n/a';
	}
	return `${Math.round(value * 100) / 100}${suffix}`;
}

function formatDelta(value, formatter) {
	if (value === null || value === undefined) {
		return 'n/a';
	}
	return `${value > 0 ? '+' : ''}${formatter(value)}`;
}

function metricLine(label, metric, formatter) {
	return `| ${label} | ${formatter(metric.baseline)} | ${formatter(metric.candidate)} | ${formatDelta(metric.delta, formatter)} |`;
}

function formatCodeboxMemoryComparisonMarkdown(comparison) {
	const lines = ['# Codebox Browser Memory Comparison', ''];
	if (!comparison.scenarios.length) {
		return `${lines.join('\n')}No matching Codebox browser memory scenarios found.\n`;
	}
	lines.push(`Status: **${comparison.status}**`, '');
	if (comparison.findings.length > 0) {
		lines.push('| Scenario | Status | Metric | Delta | Threshold |', '|---|---|---|---:|---:|');
		for (const finding of comparison.findings) {
			const formatter = finding.unit === 'bytes' ? formatBytes : (value) => formatNumber(value, finding.unit === '%' ? '%' : '');
			lines.push(`| ${finding.scenario} | ${finding.status} | ${finding.metric} | ${formatDelta(finding.delta, formatter)} | ${formatter(finding.threshold)} |`);
		}
		lines.push('');
	} else {
		lines.push('Thresholds are report-only unless configured. No configured warning or failure thresholds were exceeded.', '');
	}
	for (const scenario of comparison.scenarios) {
		lines.push(`## ${scenario.label}`, '', '| Metric | Baseline | Candidate | Delta |', '|---|---:|---:|---:|');
		lines.push(metricLine('Peak heap', scenario.metrics.peakHeapBytes, formatBytes));
		lines.push(metricLine('Post-idle retained heap', scenario.metrics.postIdleRetainedHeapBytes, formatBytes));
		lines.push(metricLine('Long task total', scenario.metrics.longTaskTotalMs, (value) => formatNumber(value, ' ms')));
		lines.push(metricLine('Resource count', scenario.metrics.resourceCount, formatNumber));
		lines.push(metricLine('Transfer size', scenario.metrics.transferSizeBytes, formatBytes));
		lines.push('');
		if (scenario.checkpoints.length > 0) {
			lines.push('| Checkpoint | Baseline Heap | Candidate Heap | Delta |', '|---|---:|---:|---:|');
			for (const checkpoint of scenario.checkpoints) {
				lines.push(`| ${checkpoint.checkpoint} | ${formatBytes(checkpoint.baselineHeapBytes)} | ${formatBytes(checkpoint.candidateHeapBytes)} | ${formatDelta(checkpoint.deltaHeapBytes, formatBytes)} |`);
			}
			lines.push('');
		}
	}
	return `${lines.join('\n')}\n`;
}

function readJsonFile(filePath) {
	return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

function runCli(argv = process.argv.slice(2)) {
	const args = new Map();
	for (let index = 0; index < argv.length; index += 2) {
		args.set(argv[index], argv[index + 1]);
	}
	const baselinePath = args.get('--baseline');
	const candidatePath = args.get('--candidate');
	if (!baselinePath || !candidatePath) {
		throw new Error('Usage: node codebox-memory-report.js --baseline <file> --candidate <file> [--thresholds-json <json>]');
	}
	const thresholdsJson = args.get('--thresholds-json') || '{}';
	const comparison = compareCodeboxMemoryResults({
		baseline: readJsonFile(baselinePath),
		candidate: readJsonFile(candidatePath),
		thresholds: JSON.parse(thresholdsJson),
	});
	process.stdout.write(formatCodeboxMemoryComparisonMarkdown(comparison));
	return comparison.status === 'failed' ? 1 : 0;
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
	DEFAULT_THRESHOLDS,
	compareCodeboxMemoryResults,
	extractCodeboxMemoryScenarios,
	formatCodeboxMemoryComparisonMarkdown,
	normalizeThresholds,
};
