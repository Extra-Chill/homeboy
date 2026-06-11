'use strict';

/**
 * External dependencies
 */
const fs = require('node:fs');

const DEFAULT_OPTIONS = Object.freeze({ parityPercent: 2, regressionPercent: 5, maxSamples: 9 });
const HIGHER_IS_BETTER_PATTERNS = [/success/i, /pass/i, /reward/i, /score/i];

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
	return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

function round(value, places = 3) {
	const number = finiteNumber(value);
	if (number === null) {
		return null;
	}
	const factor = 10 ** places;
	return Math.round(number * factor) / factor;
}

function median(values) {
	const numbers = asArray(values).map(finiteNumber).filter((value) => value !== null).sort((a, b) => a - b);
	if (numbers.length === 0) {
		return null;
	}
	const middle = Math.floor(numbers.length / 2);
	return numbers.length % 2 === 1 ? numbers[middle] : (numbers[middle - 1] + numbers[middle]) / 2;
}

function percentDelta(baseline, candidate) {
	if (baseline === null || candidate === null || baseline === 0) {
		return null;
	}
	return ((candidate - baseline) / Math.abs(baseline)) * 100;
}

function metricDirection(metric, options) {
	if (options.higherIsBetterMetricSet.has(metric)) {
		return 'higher';
	}
	if (options.lowerIsBetterMetricSet.has(metric)) {
		return 'lower';
	}
	return HIGHER_IS_BETTER_PATTERNS.some((pattern) => pattern.test(metric)) ? 'higher' : 'lower';
}

function classifyComparableMetric(row, options) {
	if (row.baseline_median === null || row.candidate_median === null || row.percent_delta === null) {
		return 'safety';
	}
	const effectiveDelta = row.direction === 'higher' ? row.percent_delta * -1 : row.percent_delta;
	if (effectiveDelta > options.regressionPercent) {
		return 'regression';
	}
	if (effectiveDelta < options.parityPercent * -1) {
		return 'improvement';
	}
	return 'parity';
}

function extractScenarios(input) {
	if (Array.isArray(input?.scenarios)) {
		return input.scenarios;
	}
	if (Array.isArray(input?.results)) {
		return input.results;
	}
	if (Array.isArray(input?.comparisons)) {
		return input.comparisons;
	}
	return [];
}

function metricSamples(value, candidateValue) {
	if (isPlainObject(value)) {
		return {
			baselineSamples: asArray(value.baseline_samples || value.baselineSamples || value.before_samples || value.beforeSamples),
			candidateSamples: asArray(value.candidate_samples || value.candidateSamples || value.after_samples || value.afterSamples),
		};
	}
	return {
		baselineSamples: Array.isArray(value) ? value : [],
		candidateSamples: Array.isArray(candidateValue) ? candidateValue : [],
	};
}

function normalizeMetricRow({ scenario, metric, value = {}, baselineValue, candidateValue, options }) {
	const samples = metricSamples(value, candidateValue);
	const baseline = finiteNumber(baselineValue)
		?? finiteNumber(value.baseline)
		?? finiteNumber(value.before)
		?? median(samples.baselineSamples);
	const candidate = finiteNumber(candidateValue)
		?? finiteNumber(value.candidate)
		?? finiteNumber(value.after)
		?? median(samples.candidateSamples)
		?? (finiteNumber(value) !== null ? value : null);
	const delta = baseline !== null && candidate !== null ? candidate - baseline : null;
	const row = {
		kind: 'measurement',
		scenario_id: scenario.id || 'unknown',
		metric,
		direction: metricDirection(metric, options),
		baseline_median: round(baseline),
		candidate_median: round(candidate),
		delta: round(delta),
		percent_delta: round(finiteNumber(value.percentDelta) ?? finiteNumber(value.percent_delta) ?? percentDelta(baseline, candidate)),
		baseline_samples: samples.baselineSamples.slice(0, options.maxSamples),
		candidate_samples: samples.candidateSamples.slice(0, options.maxSamples),
	};
	row.verdict = classifyComparableMetric(row, options);
	return row;
}

function metricNamesForScenario(baselineScenario, candidateScenario, options) {
	if (options.metricSet.size > 0) {
		return [...options.metricSet];
	}
	return [...new Set([
		...Object.keys(baselineScenario?.metrics || {}),
		...Object.keys(candidateScenario?.metrics || {}),
	])];
}

function rowsFromBaselineCandidate({ baseline, candidate, options }) {
	const baselineById = new Map(extractScenarios(baseline).map((scenario) => [scenario.id, scenario]));
	const rows = [];
	for (const candidateScenario of extractScenarios(candidate)) {
		if (options.scenarioSet.size > 0 && !options.scenarioSet.has(candidateScenario.id)) {
			continue;
		}
		const baselineScenario = baselineById.get(candidateScenario.id);
		if (!baselineScenario) {
			continue;
		}
		for (const metric of metricNamesForScenario(baselineScenario, candidateScenario, options)) {
			const baselineValue = baselineScenario.metrics?.[metric];
			const candidateValue = candidateScenario.metrics?.[metric];
			if (finiteNumber(baselineValue) === null && finiteNumber(candidateValue) === null) {
				continue;
			}
			rows.push(normalizeMetricRow({ scenario: candidateScenario, metric, baselineValue, candidateValue, options }));
		}
	}
	return rows;
}

function rowsFromCompareArtifact(input, options) {
	const rows = [];
	for (const scenario of extractScenarios(input)) {
		if (options.scenarioSet.size > 0 && !options.scenarioSet.has(scenario.id)) {
			continue;
		}
		for (const [metric, value] of Object.entries(scenario.metrics || {})) {
			if (options.metricSet.size > 0 && !options.metricSet.has(metric)) {
				continue;
			}
			if (isPlainObject(value) || finiteNumber(value) !== null) {
				rows.push(normalizeMetricRow({ scenario, metric, value, options }));
			}
		}
	}
	return rows;
}

function extractArtifacts(source, scenarioIds) {
	const artifacts = [];
	const addArtifacts = (scope, value) => {
		if (!isPlainObject(value)) {
			return;
		}
		for (const [name, artifact] of Object.entries(value)) {
			if (typeof artifact === 'string') {
				artifacts.push({ kind: 'artifact', scope, name, path: artifact });
			} else if (isPlainObject(artifact)) {
				artifacts.push({ kind: 'artifact', scope, name, ...artifact });
			}
		}
	};
	addArtifacts('run', source?.artifacts);
	for (const scenario of extractScenarios(source)) {
		if (scenarioIds.size > 0 && !scenarioIds.has(scenario.id)) {
			continue;
		}
		addArtifacts(`scenario:${scenario.id}`, scenario.artifacts);
	}
	return artifacts;
}

function summarizeVerdict(rows) {
	const comparable = rows.filter((row) => row.baseline_median !== null && row.candidate_median !== null);
	if (comparable.some((row) => row.verdict === 'regression')) {
		return 'regression';
	}
	if (comparable.some((row) => row.verdict === 'improvement')) {
		return 'improvement';
	}
	if (comparable.length > 0) {
		return 'parity';
	}
	return rows.length > 0 ? 'safety' : 'no_measurements';
}

function normalizeOptions(options) {
	const normalized = { ...DEFAULT_OPTIONS, ...options };
	if (!Number.isFinite(normalized.parityPercent)) {
		normalized.parityPercent = DEFAULT_OPTIONS.parityPercent;
	}
	if (!Number.isFinite(normalized.regressionPercent)) {
		normalized.regressionPercent = DEFAULT_OPTIONS.regressionPercent;
	}
	if (!Number.isFinite(normalized.maxSamples)) {
		normalized.maxSamples = DEFAULT_OPTIONS.maxSamples;
	}
	normalized.metricSet = new Set(asArray(normalized.metrics));
	normalized.scenarioSet = new Set(asArray(normalized.scenarios));
	normalized.higherIsBetterMetricSet = new Set(asArray(normalized.higherIsBetterMetrics));
	normalized.lowerIsBetterMetricSet = new Set(asArray(normalized.lowerIsBetterMetrics));
	return normalized;
}

function summarizeWebperfEvidence(input, options = {}) {
	const normalizedOptions = normalizeOptions(options);
	const rows = input?.baseline && input?.candidate
		? rowsFromBaselineCandidate({ baseline: input.baseline, candidate: input.candidate, options: normalizedOptions })
		: rowsFromCompareArtifact(input, normalizedOptions);
	const scenarioIds = new Set(rows.map((row) => row.scenario_id));
	const sourceForArtifacts = input?.candidate || input;
	const caveats = [];
	if (normalizedOptions.metricSet.size > 0) {
		const present = new Set(rows.map((row) => row.metric));
		for (const metric of normalizedOptions.metricSet) {
			if (!present.has(metric)) {
				caveats.push(`Requested metric not found in comparable measurements: ${metric}.`);
			}
		}
	}
	if (!rows.some((row) => row.baseline_median !== null && row.candidate_median !== null)) {
		caveats.push('No baseline/candidate metric pair was available; this summary can only support behavioral safety, not a performance win.');
	}

	return {
		schema: 'homeboy/webperf-evidence-summary/v1',
		verdict: summarizeVerdict(rows),
		metric_focus: [...normalizedOptions.metricSet],
		scenario_focus: [...normalizedOptions.scenarioSet],
		thresholds: {
			parity_percent: normalizedOptions.parityPercent,
			regression_percent: normalizedOptions.regressionPercent,
		},
		measurement_rows: rows,
		artifacts: extractArtifacts(sourceForArtifacts, scenarioIds),
		provenance: {
			component_id: input?.candidate?.component_id || input?.component_id || null,
			baseline_component_id: input?.baseline?.component_id || null,
			generated_from: input?.baseline && input?.candidate ? 'baseline-candidate-results' : 'compare-artifact',
		},
		caveats,
	};
}

function formatValue(value, suffix = '') {
	return value === null || value === undefined ? 'n/a' : `${value}${suffix}`;
}

function formatWebperfEvidenceSummaryMarkdown(summary) {
	const lines = [
		'# Web Performance Evidence Summary',
		'',
		`Verdict: **${summary.verdict}**`,
		'',
		'| Scenario | Metric | Baseline median | Candidate median | Delta | Delta % | Result |',
		'|---|---|---:|---:|---:|---:|---|',
	];
	for (const row of summary.measurement_rows) {
		lines.push(`| ${row.scenario_id} | ${row.metric} | ${formatValue(row.baseline_median)} | ${formatValue(row.candidate_median)} | ${formatValue(row.delta)} | ${formatValue(row.percent_delta, '%')} | ${row.verdict} |`);
	}
	if (summary.measurement_rows.length === 0) {
		lines.push('| n/a | n/a | n/a | n/a | n/a | n/a | no_measurements |');
	}
	if (summary.artifacts.length > 0) {
		lines.push('', '## Artifacts', '', '| Scope | Name | Path |', '|---|---|---|');
		for (const artifact of summary.artifacts) {
			lines.push(`| ${artifact.scope || 'run'} | ${artifact.name || artifact.label || 'artifact'} | ${artifact.path || artifact.url || ''} |`);
		}
	}
	if (summary.caveats.length > 0) {
		lines.push('', '## Caveats', '');
		for (const caveat of summary.caveats) {
			lines.push(`- ${caveat}`);
		}
	}
	return `${lines.join('\n')}\n`;
}

function readJsonFile(filePath) {
	return JSON.parse(fs.readFileSync(filePath, 'utf8'));
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
	if (!args.input && (!args.baseline || !args.candidate)) {
		throw new Error('Usage: node webperf-evidence-summary.js --input <compare.json> OR --baseline <baseline-results.json> --candidate <candidate-results.json>');
	}
	const input = args.baseline && args.candidate
		? { baseline: readJsonFile(args.baseline), candidate: readJsonFile(args.candidate) }
		: readJsonFile(args.input);
	const summary = summarizeWebperfEvidence(input, {
		metrics: parseJsonOption(args.metrics, undefined),
		scenarios: parseJsonOption(args.scenarios, undefined),
		higherIsBetterMetrics: parseJsonOption(args['higher-is-better'], undefined),
		lowerIsBetterMetrics: parseJsonOption(args['lower-is-better'], undefined),
		parityPercent: args['parity-percent'] ? Number(args['parity-percent']) : undefined,
		regressionPercent: args['regression-percent'] ? Number(args['regression-percent']) : undefined,
	});
	if (args['output-json']) {
		fs.writeFileSync(args['output-json'], `${JSON.stringify(summary, null, 2)}\n`);
	}
	const markdown = formatWebperfEvidenceSummaryMarkdown(summary);
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
	formatWebperfEvidenceSummaryMarkdown,
	summarizeWebperfEvidence,
};
