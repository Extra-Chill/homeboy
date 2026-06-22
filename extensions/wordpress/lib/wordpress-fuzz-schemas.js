'use strict';

const WORDPRESS_SURFACE_DISCOVERY_SCHEMA = 'wordpress-surface-discovery/v1';
const WORDPRESS_FUZZ_PLAN_SCHEMA = 'wordpress-fuzz-plan/v1';
const WORDPRESS_FUZZ_RESULT_SCHEMA = 'wordpress-fuzz-result/v1';

/**
 * Internal dependencies
 */
const {
	isWordPressSurfaceType,
	normalizeWordPressSurfaceType,
} = require('./wordpress-surface-types');

const CASE_STATUSES = new Set(['passed', 'failed', 'errored', 'skipped']);
const RESULT_STATUSES = new Set(['passed', 'failed', 'errored', 'partial', 'skipped']);
const BUDGET_METRICS = [
	{
		metric: 'request_duration_ms',
		budget: 'max_request_duration_ms',
		code: 'request_duration_budget_exceeded',
		label: 'request duration',
		budgetAliases: ['max_request_duration_ms', 'maxRequestDurationMs', 'max_duration_ms', 'maxDurationMs', 'request_duration_ms', 'duration_ms'],
		valueAliases: ['request_duration_ms', 'requestDurationMs', 'duration_ms', 'durationMs'],
	},
	{
		metric: 'query_count',
		budget: 'max_query_count',
		code: 'query_count_budget_exceeded',
		label: 'query count',
		budgetAliases: ['max_query_count', 'maxQueryCount', 'query_count', 'queryCount'],
		valueAliases: ['query_count', 'queryCount'],
	},
	{
		metric: 'memory_peak_bytes',
		budget: 'max_memory_peak_bytes',
		code: 'memory_peak_budget_exceeded',
		label: 'peak memory',
		budgetAliases: ['max_memory_peak_bytes', 'maxMemoryPeakBytes', 'memory_peak_bytes', 'memoryPeakBytes', 'peak_bytes', 'peakBytes'],
		valueAliases: ['memory_peak_bytes', 'memoryPeakBytes', 'peak_bytes', 'peakBytes'],
	},
	{
		metric: 'browser_resource_count',
		budget: 'max_browser_resource_count',
		code: 'browser_resource_count_budget_exceeded',
		label: 'browser resource count',
		budgetAliases: ['max_browser_resource_count', 'maxBrowserResourceCount', 'max_resource_count', 'maxResourceCount', 'browser_resource_count', 'browserResourceCount', 'resource_count', 'resourceCount'],
		valueAliases: ['browser_resource_count', 'browserResourceCount', 'resource_count', 'resourceCount', 'count'],
	},
];

function normalizeWordPressFuzzSurfaceType(value) {
	return normalizeWordPressSurfaceType(value);
}

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
	const type = normalizeWordPressSurfaceType(surface.type || surface.kind, { allowUnknown: true });
	if (!type || typeof type !== 'string') {
		throw new Error(`surfaces[${index}].type must be a string.`);
	}
	if (!isWordPressSurfaceType(type)) {
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
	const normalized = {
		...testCase,
		id: normalizeId(testCase.id, `case-${index + 1}`, `${targetField}.cases[${index}].id`),
		operation_id: testCase.operation_id || testCase.operationId || testCase.operation?.id || null,
		budget: normalizePerformanceBudget(testCase.budget || testCase.budgets || testCase.performance_budget || testCase.performanceBudget),
		metadata: { ...(testCase.metadata || {}) },
	};
	const requiredCapabilities = normalizeCapabilityCodes(testCase.required_capabilities || testCase.requiredCapabilities);
	if (requiredCapabilities.length > 0) {
		normalized.required_capabilities = requiredCapabilities;
	}
	return normalized;
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
		operation_id: target.operation_id || target.operationId || null,
		budget: normalizePerformanceBudget(target.budget || target.budgets || target.performance_budget || target.performanceBudget),
		cases: asArray(target.cases, `${field}.cases`).map((testCase, caseIndex) => normalizeFuzzCase(testCase, caseIndex, field)),
		metadata: { ...(target.metadata || {}) },
	};
}

function normalizeReasonCodes(value) {
	if (value === undefined || value === null) {
		return [];
	}
	return [...new Set(asArray(Array.isArray(value) ? value : [value], 'reason_codes').map(String).filter(Boolean))].sort();
}

function normalizeCapabilityCodes(value) {
	if (value === undefined || value === null) {
		return [];
	}
	return [...new Set(asArray(Array.isArray(value) ? value : [value], 'required_capabilities').map(String).filter(Boolean))].sort();
}

function normalizeWordPressFuzzPlan(plan) {
	assertPlainObject(plan, 'plan');
	assertSchema(plan.schema, WORDPRESS_FUZZ_PLAN_SCHEMA, 'WordPress fuzz plan');

	return {
		schema: WORDPRESS_FUZZ_PLAN_SCHEMA,
		id: normalizeId(plan.id, 'wordpress-fuzz-plan', 'plan.id'),
		discovery_id: plan.discovery_id || plan.discoveryId || null,
		targets: asArray(plan.targets, 'targets').map(normalizeFuzzTarget),
		budget: normalizePerformanceBudget(plan.budget || plan.budgets || plan.performance_budget || plan.performanceBudget),
		metadata: { ...(plan.metadata || {}) },
	};
}

function normalizeFuzzCaseResult(result, index) {
	assertPlainObject(result, `cases[${index}]`);
	const budget = normalizePerformanceBudget(result.budget || result.budgets || result.performance_budget || result.performanceBudget || result.metadata?.budget || result.metadata?.budgets);
	const metrics = normalizeCasePerformanceMetrics(result);
	const budgetFindings = normalizeBudgetFindings({ budget, metrics, subject: result.id || `case-${index + 1}` });
	const status = normalizeCaseStatus(result.status, budgetFindings);
	if (!CASE_STATUSES.has(status)) {
		throw new Error(`Unsupported WordPress fuzz case status: ${status}`);
	}
	const findings = [...asOptionalArray(result.findings, `cases[${index}].findings`), ...budgetFindings];
	const diagnostics = [...asOptionalArray(result.diagnostics, `cases[${index}].diagnostics`), ...budgetFindings.map(findingToDiagnostic)];

	return {
		...result,
		id: normalizeId(result.id, `case-${index + 1}`, `cases[${index}].id`),
		target_id: result.target_id || result.targetId || null,
		surface_id: result.surface_id || result.surfaceId || null,
		operation_id: result.operation_id || result.operationId || result.operation?.id || null,
		status,
		skip_reason: result.skip_reason || result.skipReason || null,
		skip_reasons: normalizeReasonCodes(result.skip_reasons || result.skipReasons || result.skip_reason || result.skipReason),
		destructive_reason: result.destructive_reason || result.destructiveReason || null,
		destructive_reasons: normalizeReasonCodes(result.destructive_reasons || result.destructiveReasons || result.destructive_reason || result.destructiveReason),
		role_boundary: result.role_boundary || result.roleBoundary || null,
		db_query: result.db_query || result.dbQuery || null,
		admin_browser: result.admin_browser || result.adminBrowser || null,
		http_guardrail: result.http_guardrail || result.httpGuardrail || null,
		duration_ms: Number.isFinite(result.duration_ms) ? result.duration_ms : result.durationMs || null,
		budget,
		performance_metrics: metrics,
		findings,
		diagnostics,
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

function countUnique(cases, field) {
	return [...new Set(cases.map((result) => result[field]).filter(Boolean))].length;
}

function countReasonCodes(cases, field) {
	return cases.reduce((counts, result) => {
		for (const reason of result[field] || []) {
			counts[reason] = (counts[reason] || 0) + 1;
		}
		return counts;
	}, {});
}

function summarizeRoleBoundaries(cases) {
	return cases.reduce((summary, result) => {
		if (!result.role_boundary) {
			return summary;
		}
		summary.total += 1;
		const outcome = String(result.role_boundary.outcome || result.role_boundary.status || result.status || 'unknown');
		summary.by_outcome[outcome] = (summary.by_outcome[outcome] || 0) + 1;
		return summary;
	}, { total: 0, by_outcome: {} });
}

function summarizeDbQueries(cases) {
	return cases.reduce((summary, result) => {
		const query = result.db_query;
		if (!query) {
			return summary;
		}
		summary.total += 1;
		summary.query_count += Number(query.query_count ?? query.queryCount ?? 0) || 0;
		summary.rows_examined += Number(query.rows_examined ?? query.rowsExamined ?? 0) || 0;
		summary.duration_ms += Number(query.duration_ms ?? query.durationMs ?? 0) || 0;
		return summary;
	}, { total: 0, query_count: 0, rows_examined: 0, duration_ms: 0 });
}

function summarizeNestedCases(cases, field) {
	return cases.reduce((summary, result) => {
		const value = result[field];
		if (!value) {
			return summary;
		}
		summary.total += 1;
		if (value.errors !== undefined) {
			summary.errors += Array.isArray(value.errors) ? value.errors.length : Number(value.errors) || 0;
		}
		if (value.blocked !== undefined) {
			summary.blocked += Number(value.blocked) || (value.blocked === true ? 1 : 0);
		}
		if (value.allowed !== undefined) {
			summary.allowed += Number(value.allowed) || (value.allowed === true ? 1 : 0);
		}
		return summary;
	}, { total: 0, errors: 0, blocked: 0, allowed: 0 });
}

function summarizePerformanceMetrics(cases) {
	return {
		request_duration_ms: sumNumbers(cases.map((result) => result.performance_metrics?.request_duration_ms)),
		query_count: sumNumbers(cases.map((result) => result.performance_metrics?.query_count)),
		memory_peak_bytes: maxNumber(cases.map((result) => result.performance_metrics?.memory_peak_bytes)),
		browser_resource_count: sumNumbers(cases.map((result) => result.performance_metrics?.browser_resource_count)),
	};
}

function countBudgetFindings(cases) {
	return cases.reduce((count, result) => count + (result.findings || []).filter((finding) => finding.kind === 'performance_budget').length, 0);
}

function normalizePerformanceBudget(input) {
	if (!input || typeof input !== 'object' || Array.isArray(input)) {
		return {};
	}
	return BUDGET_METRICS.reduce((budget, metric) => {
		const value = firstNumber(input, metric.budgetAliases);
		if (value !== null) {
			budget[metric.budget] = value;
		}
		return budget;
	}, { ...input });
}

function normalizeCasePerformanceMetrics(result) {
	const sources = [
		result,
		result.metrics,
		result.performance_metrics || result.performanceMetrics,
		result.metadata?.metrics,
		result.db_query || result.dbQuery,
		result.memory,
		result.admin_browser || result.adminBrowser,
		result.browser_metrics || result.browserMetrics,
		(result.admin_browser || result.adminBrowser)?.resources,
	];

	return BUDGET_METRICS.reduce((metrics, metric) => {
		const value = firstNumberFromSources(sources, metric.valueAliases);
		if (value !== null) {
			metrics[metric.metric] = value;
		}
		return metrics;
	}, {});
}

function normalizeBudgetFindings({ budget, metrics, subject }) {
	return BUDGET_METRICS.flatMap((metric) => {
		const limit = budget[metric.budget];
		const actual = metrics[metric.metric];
		if (!Number.isFinite(limit) || !Number.isFinite(actual) || actual <= limit) {
			return [];
		}
		return [{
			kind: 'performance_budget',
			code: metric.code,
			severity: 'failure',
			subject,
			metric: metric.metric,
			actual,
			budget: limit,
			message: `WordPress fuzz ${metric.label} exceeded budget: ${actual} > ${limit}.`,
		}];
	});
}

function normalizeCaseStatus(status, budgetFindings) {
	if (budgetFindings.length > 0 && (!status || status === 'passed')) {
		return 'failed';
	}
	return status || 'errored';
}

function findingToDiagnostic(finding) {
	return {
		severity: finding.severity,
		code: finding.code,
		message: finding.message,
		metric: finding.metric,
		actual: finding.actual,
		budget: finding.budget,
		subject: finding.subject,
	};
}

function firstNumberFromSources(sources, keys) {
	for (const source of sources) {
		const value = firstNumber(source, keys);
		if (value !== null) {
			return value;
		}
	}
	return null;
}

function firstNumber(source, keys) {
	if (!source || typeof source !== 'object' || Array.isArray(source)) {
		return null;
	}
	for (const key of keys) {
		const value = numberOrNull(source[key]);
		if (value !== null) {
			return value;
		}
	}
	return null;
}

function sumNumbers(values) {
	return values.reduce((sum, value) => sum + (Number(value) || 0), 0);
}

function maxNumber(values) {
	const finite = values.map(Number).filter(Number.isFinite);
	return finite.length > 0 ? Math.max(...finite) : 0;
}

function normalizeProvenance(result) {
	const provenance = result.provenance || result.workload_manifest || result.workloadManifest || result.manifest || null;
	if (!provenance || typeof provenance !== 'object' || Array.isArray(provenance)) {
		return provenance ? { workload_manifest: String(provenance) } : null;
	}
	return {
		workload_manifest: provenance.workload_manifest || provenance.workloadManifest || provenance.path || provenance.file || provenance.id || null,
		workload_id: provenance.workload_id || provenance.workloadId || null,
		discovery_id: provenance.discovery_id || provenance.discoveryId || null,
	};
}

function normalizeWordPressFuzzResult(result) {
	assertPlainObject(result, 'result');
	assertSchema(result.schema, WORDPRESS_FUZZ_RESULT_SCHEMA, 'WordPress fuzz result');

	const cases = asArray(result.cases, 'cases').map(normalizeFuzzCaseResult);
	const caseSummary = summarizeCases(cases);
	const performanceMetrics = summarizePerformanceMetrics(cases);
	const resultBudget = normalizePerformanceBudget(result.budget || result.budgets || result.performance_budget || result.performanceBudget || result.metadata?.budget || result.metadata?.budgets);
	const resultBudgetFindings = normalizeBudgetFindings({ budget: resultBudget, metrics: performanceMetrics, subject: result.id || 'wordpress-fuzz-result' });
	const summary = {
		...caseSummary,
		case_counts: { ...caseSummary, ...(result.summary?.case_counts || result.summary?.caseCounts || {}) },
		surface_count: countUnique(cases, 'surface_id'),
		operation_count: countUnique(cases, 'operation_id'),
		skipped_reason_codes: countReasonCodes(cases, 'skip_reasons'),
		destructive_reason_codes: countReasonCodes(cases, 'destructive_reasons'),
		role_boundary_outcomes: summarizeRoleBoundaries(cases),
		db_query_metrics: summarizeDbQueries(cases),
		admin_browser_errors: summarizeNestedCases(cases, 'admin_browser'),
		http_guardrail_outcomes: summarizeNestedCases(cases, 'http_guardrail'),
		performance_metrics: performanceMetrics,
		budget_failure_count: countBudgetFindings(cases) + resultBudgetFindings.length,
		...(result.summary || {}),
	};
	const status = result.status || (summary.failed || summary.errored || summary.budget_failure_count ? 'failed' : 'passed');
	if (!RESULT_STATUSES.has(status)) {
		throw new Error(`Unsupported WordPress fuzz result status: ${status}`);
	}
	const findings = [
		...asOptionalArray(result.findings, 'findings'),
		...cases.flatMap((testCase) => (testCase.findings || []).filter((finding) => finding.kind === 'performance_budget')),
		...resultBudgetFindings,
	];
	const diagnostics = [
		...asOptionalArray(result.diagnostics, 'diagnostics'),
		...cases.flatMap((testCase) => (testCase.diagnostics || []).filter((diagnostic) => String(diagnostic.code || '').endsWith('_budget_exceeded'))),
		...resultBudgetFindings.map(findingToDiagnostic),
	];

	return {
		schema: WORDPRESS_FUZZ_RESULT_SCHEMA,
		id: normalizeId(result.id, 'wordpress-fuzz-result', 'result.id'),
		plan_id: result.plan_id || result.planId || null,
		status,
		started_at: result.started_at || result.startedAt || null,
		finished_at: result.finished_at || result.finishedAt || null,
		summary,
		cases,
		budget: resultBudget,
		findings,
		diagnostics,
		artifacts: asArray(result.artifacts, 'artifacts'),
		provenance: normalizeProvenance(result),
		metadata: { ...(result.metadata || {}) },
	};
}

function asOptionalArray(value, field) {
	if (value === undefined || value === null) {
		return [];
	}
	return asArray(value, field);
}

function numberOrNull(value) {
	if (value === undefined || value === null || value === '') {
		return null;
	}
	const parsed = Number(value);
	return Number.isFinite(parsed) ? parsed : null;
}

module.exports = {
	WORDPRESS_SURFACE_DISCOVERY_SCHEMA,
	WORDPRESS_FUZZ_PLAN_SCHEMA,
	WORDPRESS_FUZZ_RESULT_SCHEMA,
	normalizeWordPressFuzzSurfaceType,
	normalizeWordPressSurfaceDiscovery,
	normalizeWordPressFuzzPlan,
	normalizeWordPressFuzzResult,
};
