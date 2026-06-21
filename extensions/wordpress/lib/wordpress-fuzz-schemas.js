'use strict';

const WORDPRESS_SURFACE_DISCOVERY_SCHEMA = 'wordpress-surface-discovery/v1';
const WORDPRESS_FUZZ_PLAN_SCHEMA = 'wordpress-fuzz-plan/v1';
const WORDPRESS_FUZZ_RESULT_SCHEMA = 'wordpress-fuzz-result/v1';

const SURFACE_TYPES = new Set([
	'admin-page',
	'block',
	'capability',
	'cron-event',
	'database-table',
	'frontend-url',
	'hook',
	'option',
	'post-type',
	'rest-route',
	'role',
	'taxonomy',
	'wp-cli-command',
]);

const CASE_STATUSES = new Set(['passed', 'failed', 'errored', 'skipped']);
const RESULT_STATUSES = new Set(['passed', 'failed', 'errored', 'partial', 'skipped']);
const SURFACE_TYPE_ALIASES = new Map([
	['rest', 'rest-route'],
	['wp-cli', 'wp-cli-command'],
]);

function assertPlainObject(value, field) {
	if (!value || typeof value !== 'object' || Array.isArray(value)) {
		throw new Error(`${field} must be an object.`);
	}
}

function asArray(value, field) {
	if (value === undefined || value === null) {
		return [];
	}
	if (!Array.isArray(value)) {
		throw new Error(`${field} must be an array.`);
	}
	return value;
}

function assertSchema(value, expected, field) {
	if (value && value !== expected) {
		throw new Error(`Unsupported ${field} schema: ${value}`);
	}
}

function normalizeId(value, fallback, field) {
	const id = value || fallback;
	if (!id || typeof id !== 'string') {
		throw new Error(`${field} must be a string.`);
	}
	return id;
}

function normalizeSurface(surface, index) {
	assertPlainObject(surface, `surfaces[${index}]`);
	const type = SURFACE_TYPE_ALIASES.get(surface.type || surface.kind) || surface.type || surface.kind;
	if (!type || typeof type !== 'string') {
		throw new Error(`surfaces[${index}].type must be a string.`);
	}
	if (!SURFACE_TYPES.has(type)) {
		throw new Error(`Unsupported WordPress surface type: ${type}`);
	}

	return {
		...surface,
		id: normalizeId(surface.id, `${type}-${index + 1}`, `surfaces[${index}].id`),
		type,
		label: surface.label || surface.name || surface.id || `${type} ${index + 1}`,
		metadata: { ...(surface.metadata || {}) },
	};
}

function normalizeWordPressSurfaceDiscovery(discovery) {
	assertPlainObject(discovery, 'discovery');
	assertSchema(discovery.schema, WORDPRESS_SURFACE_DISCOVERY_SCHEMA, 'WordPress surface discovery');

	const surfaces = asArray(discovery.surfaces, 'surfaces').map(normalizeSurface);
	return {
		schema: WORDPRESS_SURFACE_DISCOVERY_SCHEMA,
		id: normalizeId(discovery.id, 'wordpress-surface-discovery', 'discovery.id'),
		label: discovery.label || discovery.id || 'WordPress surface discovery',
		generated_at: discovery.generated_at || discovery.generatedAt || null,
		source: discovery.source || 'wordpress',
		surfaces,
		metadata: { ...(discovery.metadata || {}) },
	};
}

function normalizeFuzzCase(testCase, index, targetField) {
	assertPlainObject(testCase, `${targetField}.cases[${index}]`);
	return {
		...testCase,
		id: normalizeId(testCase.id, `case-${index + 1}`, `${targetField}.cases[${index}].id`),
		metadata: { ...(testCase.metadata || {}) },
	};
}

function normalizeFuzzTarget(target, index) {
	assertPlainObject(target, `targets[${index}]`);
	const field = `targets[${index}]`;
	const id = normalizeId(target.id, target.surface_id || target.surfaceId || `target-${index + 1}`, `${field}.id`);
	const surfaceId = target.surface_id || target.surfaceId || id;
	if (!surfaceId || typeof surfaceId !== 'string') {
		throw new Error(`${field}.surface_id must be a string.`);
	}

	return {
		...target,
		id,
		surface_id: surfaceId,
		cases: asArray(target.cases, `${field}.cases`).map((testCase, caseIndex) => normalizeFuzzCase(testCase, caseIndex, field)),
		metadata: { ...(target.metadata || {}) },
	};
}

function normalizeWordPressFuzzPlan(plan) {
	assertPlainObject(plan, 'plan');
	assertSchema(plan.schema, WORDPRESS_FUZZ_PLAN_SCHEMA, 'WordPress fuzz plan');

	return {
		schema: WORDPRESS_FUZZ_PLAN_SCHEMA,
		id: normalizeId(plan.id, 'wordpress-fuzz-plan', 'plan.id'),
		discovery_id: plan.discovery_id || plan.discoveryId || null,
		targets: asArray(plan.targets, 'targets').map(normalizeFuzzTarget),
		budget: { ...(plan.budget || {}) },
		metadata: { ...(plan.metadata || {}) },
	};
}

function normalizeFuzzCaseResult(result, index) {
	assertPlainObject(result, `cases[${index}]`);
	const status = result.status || 'errored';
	if (!CASE_STATUSES.has(status)) {
		throw new Error(`Unsupported WordPress fuzz case status: ${status}`);
	}

	return {
		...result,
		id: normalizeId(result.id, `case-${index + 1}`, `cases[${index}].id`),
		target_id: result.target_id || result.targetId || null,
		status,
		duration_ms: Number.isFinite(result.duration_ms) ? result.duration_ms : result.durationMs || null,
		metadata: { ...(result.metadata || {}) },
	};
}

function summarizeCases(cases) {
	return cases.reduce((summary, result) => {
		summary.total += 1;
		summary[result.status] += 1;
		return summary;
	}, { total: 0, passed: 0, failed: 0, errored: 0, skipped: 0 });
}

function normalizeWordPressFuzzResult(result) {
	assertPlainObject(result, 'result');
	assertSchema(result.schema, WORDPRESS_FUZZ_RESULT_SCHEMA, 'WordPress fuzz result');

	const cases = asArray(result.cases, 'cases').map(normalizeFuzzCaseResult);
	const summary = { ...summarizeCases(cases), ...(result.summary || {}) };
	const status = result.status || (summary.failed || summary.errored ? 'failed' : 'passed');
	if (!RESULT_STATUSES.has(status)) {
		throw new Error(`Unsupported WordPress fuzz result status: ${status}`);
	}

	return {
		schema: WORDPRESS_FUZZ_RESULT_SCHEMA,
		id: normalizeId(result.id, 'wordpress-fuzz-result', 'result.id'),
		plan_id: result.plan_id || result.planId || null,
		status,
		started_at: result.started_at || result.startedAt || null,
		finished_at: result.finished_at || result.finishedAt || null,
		summary,
		cases,
		artifacts: asArray(result.artifacts, 'artifacts'),
		metadata: { ...(result.metadata || {}) },
	};
}

module.exports = {
	WORDPRESS_SURFACE_DISCOVERY_SCHEMA,
	WORDPRESS_FUZZ_PLAN_SCHEMA,
	WORDPRESS_FUZZ_RESULT_SCHEMA,
	normalizeWordPressSurfaceDiscovery,
	normalizeWordPressFuzzPlan,
	normalizeWordPressFuzzResult,
};
