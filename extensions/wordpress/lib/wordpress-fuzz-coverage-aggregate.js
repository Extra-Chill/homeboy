'use strict';

/**
 * Internal dependencies
 */
const { isPlainObject } = require('./shared');

const STATUS_ORDER = ['discovered', 'exercised', 'skipped', 'failed'];
const STATUS_PRIORITY = Object.fromEntries(STATUS_ORDER.map((status, index) => [status, index]));

function stringValue(value, fallback = '') {
	const normalized = String(value ?? '').trim();
	return normalized || fallback;
}

function numericValue(value, fallback = 0) {
	const number = Number(value);
	return Number.isFinite(number) ? number : fallback;
}

function normalizeStatus(value, fallback = 'discovered') {
	const status = stringValue(value).toLowerCase().replace(/_/g, '-');
	if (status === 'covered' || status === 'passed' || status === 'executed') {
		return 'exercised';
	}
	if (status === 'skip') {
		return 'skipped';
	}
	if (status === 'fail' || status === 'error') {
		return 'failed';
	}
	return STATUS_PRIORITY[status] === undefined ? fallback : status;
}

function normalizeFuzzCoverageItem(raw, defaults = {}) {
	const source = stringValue(defaults.source, 'input');
	const status = normalizeStatus(isPlainObject(raw) ? raw.status : '', defaults.status || 'discovered');
	if (typeof raw === 'string') {
		return {
			id: raw,
			type: stringValue(defaults.type, 'generic'),
			label: raw,
			status,
			sources: [source],
			count: 1,
		};
	}
	if (!isPlainObject(raw)) {
		throw new TypeError('WordPress fuzz coverage items must be objects or strings');
	}

	const type = stringValue(raw.type || raw.kind || raw.category || defaults.type, 'generic');
	const label = stringValue(raw.label || raw.name || raw.title || raw.id || raw.key || raw.path, type);
	const id = stringValue(raw.id || raw.key || raw.name || raw.path || `${type}:${label}`);
	return {
		id,
		type,
		label,
		status,
		sources: [stringValue(raw.source || raw.artifact || source, source)],
		count: numericValue(raw.count ?? raw.total ?? raw.value, 1),
		detail: isPlainObject(raw.detail) ? raw.detail : {},
	};
}

function collectWordPressFuzzCoverageItems(input = {}, source = 'input') {
	const artifacts = Array.isArray(input) ? input : [input];
	return artifacts.flatMap((artifact, index) => collectArtifactItems(artifact, `${source}:${index + 1}`));
}

function collectArtifactItems(artifact, source) {
	if (!isPlainObject(artifact)) {
		return [];
	}
	const items = [];
	appendStatusItems(items, artifact.items || artifact.coverageItems || artifact.coverage_items, source, 'discovered');
	for (const status of STATUS_ORDER) {
		appendStatusItems(items, artifact[status], source, status);
	}
	appendStatusItems(items, artifact.coverage?.items, source, 'discovered');
	for (const status of STATUS_ORDER) {
		appendStatusItems(items, artifact.coverage?.[status], source, status);
	}
	appendWordPressFuzzSchemaItems(items, artifact, source);
	return items;
}

function appendStatusItems(items, value, source, status) {
	if (Array.isArray(value)) {
		for (const item of value) {
			items.push(normalizeFuzzCoverageItem(item, { source, status }));
		}
		return;
	}
	if (!isPlainObject(value)) {
		return;
	}
	for (const [key, item] of Object.entries(value)) {
		items.push(normalizeFuzzCoverageItem(isPlainObject(item) ? { key, ...item } : { key, count: item }, { source, status }));
	}
}

function appendCountMap(items, map, source, status, type) {
	if (!isPlainObject(map)) {
		return;
	}
	for (const [key, count] of Object.entries(map)) {
		items.push(normalizeFuzzCoverageItem({ id: `${type}:${key}`, type, label: key, count }, { source, status }));
	}
}

function appendWordPressFuzzSchemaItems(items, artifact, source) {
	if (artifact.schema !== 'homeboy/wordpress-fuzz-coverage/v1') {
		return;
	}
	appendCountMap(items, artifact.hooks?.actions, source, 'exercised', 'action');
	appendCountMap(items, artifact.hooks?.filters, source, 'exercised', 'filter');
	if (!isPlainObject(artifact.hooks?.actions) && !isPlainObject(artifact.hooks?.filters)) {
		appendCountMap(items, artifact.hooks?.all, source, 'exercised', 'hook');
	}
	appendCountMap(items, artifact.db?.operations, source, 'exercised', 'db_operation');
	appendCountMap(items, artifact.db?.tables, source, 'exercised', 'db_table');
	appendCountMap(items, artifact.db?.categories, source, 'exercised', 'db_category');
	appendCountMap(items, artifact.mutations?.table_row_deltas, source, 'exercised', 'table_mutation');
	appendCountMap(items, artifact.mutations?.write_operations, source, 'exercised', 'write_operation');
	appendCountMap(items, artifact.php_errors?.by_kind, source, 'failed', 'php_error');
	appendCoverageGapItems(items, artifact.coverage_gaps, source);
}

function appendCoverageGapItems(items, gaps, source) {
	if (Array.isArray(gaps)) {
		for (const gap of gaps) {
			items.push(normalizeFuzzCoverageItem(gap, { source, status: 'skipped', type: 'coverage_gap' }));
		}
		return;
	}
	if (!isPlainObject(gaps)) {
		return;
	}
	for (const [key, gap] of Object.entries(gaps)) {
		if (Array.isArray(gap)) {
			for (const item of gap) {
				items.push(normalizeFuzzCoverageItem(isPlainObject(item) ? { type: key, ...item } : { type: key, label: item }, { source, status: 'skipped' }));
			}
			continue;
		}
		items.push(normalizeFuzzCoverageItem(isPlainObject(gap) ? { key, ...gap } : { key, count: gap }, { source, status: 'skipped', type: 'coverage_gap' }));
	}
}

function aggregateWordPressFuzzCoverage(input = {}) {
	const items = collectWordPressFuzzCoverageItems(input.artifacts || input.results || input.coverage || input);
	const byId = new Map();
	for (const item of items) {
		const existing = byId.get(item.id);
		if (!existing) {
			byId.set(item.id, { ...item });
			continue;
		}
		existing.count += item.count;
		existing.sources = [...new Set([...existing.sources, ...item.sources])].sort();
		if (STATUS_PRIORITY[item.status] > STATUS_PRIORITY[existing.status]) {
			existing.status = item.status;
			existing.label = item.label || existing.label;
			existing.type = item.type || existing.type;
		}
	}

	const coverageItems = [...byId.values()].sort((a, b) => a.type.localeCompare(b.type) || a.id.localeCompare(b.id));
	const totals = Object.fromEntries(STATUS_ORDER.map((status) => [status, coverageItems.filter((item) => item.status === status).length]));
	totals.total = coverageItems.length;
	totals.coveragePercent = totals.discovered + totals.exercised === 0
		? 0
		: Math.round((totals.exercised / (totals.discovered + totals.exercised)) * 10000) / 100;

	return {
		schema: 'homeboy/wordpress-fuzz-coverage-aggregate/v1',
		type: 'wordpress-fuzz-coverage-aggregate',
		totals,
		byType: buildByType(coverageItems),
		items: coverageItems,
		gapReport: buildGapReport(coverageItems),
	};
}

function buildByType(items) {
	const byType = {};
	for (const item of items) {
		if (!byType[item.type]) {
			byType[item.type] = Object.fromEntries(STATUS_ORDER.map((status) => [status, 0]));
			byType[item.type].total = 0;
		}
		byType[item.type][item.status] += 1;
		byType[item.type].total += 1;
	}
	return Object.fromEntries(Object.entries(byType).sort(([a], [b]) => a.localeCompare(b)));
}

function buildGapReport(items) {
	const gapItems = items.filter((item) => item.status !== 'exercised');
	return {
		schema: 'homeboy/wordpress-fuzz-coverage-gap-report/v1',
		totals: Object.fromEntries(STATUS_ORDER.filter((status) => status !== 'exercised').map((status) => [status, gapItems.filter((item) => item.status === status).length])),
		items: gapItems,
	};
}

function escapeMarkdownCell(value) {
	return String(value ?? '').replace(/\|/g, '\\|').replace(/\n/g, ' ');
}

function formatWordPressFuzzCoverageMarkdownReport(input = {}, options = {}) {
	const aggregate = input?.schema === 'homeboy/wordpress-fuzz-coverage-aggregate/v1'
		? input
		: aggregateWordPressFuzzCoverage(input);
	const lines = [
		`## ${options.title || 'WordPress fuzz coverage'}`,
		'',
		`Discovered: ${aggregate.totals.discovered}; exercised: ${aggregate.totals.exercised}; skipped: ${aggregate.totals.skipped}; failed: ${aggregate.totals.failed}; coverage: ${aggregate.totals.coveragePercent}%`,
		'',
		'| Type | Discovered | Exercised | Skipped | Failed | Total |',
		'| --- | ---: | ---: | ---: | ---: | ---: |',
	];
	for (const [type, counts] of Object.entries(aggregate.byType)) {
		lines.push(`| ${escapeMarkdownCell(type)} | ${counts.discovered} | ${counts.exercised} | ${counts.skipped} | ${counts.failed} | ${counts.total} |`);
	}
	if (aggregate.gapReport.items.length > 0) {
		lines.push('', '## Gaps', '', '| Status | Type | Item | Sources |', '| --- | --- | --- | --- |');
		for (const item of aggregate.gapReport.items) {
			lines.push(`| ${item.status} | ${escapeMarkdownCell(item.type)} | ${escapeMarkdownCell(item.label || item.id)} | ${escapeMarkdownCell(item.sources.join(', '))} |`);
		}
	}
	return lines.join('\n');
}

module.exports = {
	aggregateWordPressFuzzCoverage,
	collectWordPressFuzzCoverageItems,
	formatWordPressFuzzCoverageMarkdownReport,
	normalizeWordPressFuzzCoverageItem: normalizeFuzzCoverageItem,
};
