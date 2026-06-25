'use strict';

const assert = require('node:assert/strict');
const {
	WORDPRESS_SURFACE_DISCOVERY_SCHEMA,
	WORDPRESS_FUZZ_PLAN_SCHEMA,
	WORDPRESS_FUZZ_RESULT_SCHEMA,
	normalizeWordPressFuzzSurfaceType,
	normalizeWordPressSurfaceDiscovery,
	normalizeWordPressFuzzPlan,
	normalizeWordPressFuzzResult,
} = require('../lib/wordpress-fuzz-schemas');

const discovery = normalizeWordPressSurfaceDiscovery({
	schema: WORDPRESS_SURFACE_DISCOVERY_SCHEMA,
	id: 'site-surfaces',
	surfaces: [
		{ type: 'rest-route', id: 'wp-v2-posts', method: 'GET', route: '/wp/v2/posts' },
		{ type: 'admin-page', path: '/wp-admin/edit.php', capability: 'edit_posts' },
		{ type: 'ajax_action', id: 'ajax-heartbeat', action: 'heartbeat' },
		{ kind: 'db-query', id: 'query-posts', query: 'SELECT * FROM wp_posts' },
		{ kind: 'external_http', id: 'http-api', url: 'https://api.example.test/' },
	],
});

assert.equal(discovery.schema, 'wordpress-surface-discovery/v1');
assert.equal(discovery.surfaces[0].id, 'wp-v2-posts');
assert.equal(discovery.surfaces[1].id, 'admin-page-2');
assert.equal(discovery.surfaces[2].type, 'ajax-action');
assert.equal(discovery.surfaces[3].type, 'db-query');
assert.equal(discovery.surfaces[4].type, 'external-http');
assert.equal(normalizeWordPressFuzzSurfaceType('rest_route'), 'rest-route');
assert.equal(normalizeWordPressFuzzSurfaceType('admin_page'), 'admin-page');
assert.equal(normalizeWordPressFuzzSurfaceType('ajax_action'), 'ajax-action');
assert.equal(normalizeWordPressFuzzSurfaceType('db_table'), 'database-table');
assert.equal(normalizeWordPressFuzzSurfaceType('database_query'), 'db-query');

const plan = normalizeWordPressFuzzPlan({
	schema: WORDPRESS_FUZZ_PLAN_SCHEMA,
	id: 'rest-route-fuzz',
	discovery_id: discovery.id,
	targets: [
		{
			id: 'posts-list-query',
			surface_id: 'wp-v2-posts',
			operation_id: 'rest:get-posts',
			method: 'GET',
			route: '/wp/v2/posts',
			cases: [
				{ id: 'per-page-max', operation_id: 'rest:get-posts:per-page', query: { per_page: 100 } },
				{ query: { search: '<script>' }, requiredCapabilities: ['transaction', 'snapshot', 'snapshot'] },
			],
		},
	],
	budget: { max_cases: 25 },
});

assert.equal(plan.schema, 'wordpress-fuzz-plan/v1');
assert.equal(plan.discovery_id, 'site-surfaces');
assert.equal(plan.targets[0].operation_id, 'rest:get-posts');
assert.equal(plan.targets[0].cases[0].operation_id, 'rest:get-posts:per-page');
assert.equal(plan.targets[0].cases[1].id, 'case-2');
assert.deepEqual(plan.targets[0].cases[1].required_capabilities, ['snapshot', 'transaction']);
assert.equal(plan.budget.max_cases, 25);

const result = normalizeWordPressFuzzResult({
	schema: WORDPRESS_FUZZ_RESULT_SCHEMA,
	id: 'rest-route-fuzz-result',
	plan_id: plan.id,
	cases: [
		{
			id: 'per-page-max',
			target_id: 'posts-list-query',
			surface_id: 'wp-v2-posts',
			operation_id: 'rest:get-posts:per-page',
			status: 'passed',
			duration_ms: 42,
			role_boundary: { role: 'subscriber', outcome: 'denied_as_expected' },
			db_query: { query_count: 3, rows_examined: 12, duration_ms: 5 },
			http_guardrail: { blocked: 1, allowed: 0 },
		},
		{
			id: 'case-2',
			target_id: 'posts-list-query',
			surface_id: 'wp-v2-posts',
			operation_id: 'rest:get-posts:search',
			status: 'failed',
			error: { message: '500 response' },
			admin_browser: { errors: [{ message: 'console error' }] },
		},
		{ id: 'unsafe-delete', target_id: 'posts-list-query', surface_id: 'wp-v2-posts', status: 'skipped', skip_reason: 'destructive_guardrail', destructive_reason: 'mutating_method' },
	],
	provenance: { workload_manifest: 'fuzz-manifest.json', workload_id: 'generic-workload', discovery_id: discovery.id },
});

assert.equal(result.schema, 'wordpress-fuzz-result/v1');
assert.equal(result.status, 'failed');
assert.equal(result.summary.total, 3);
assert.equal(result.summary.case_counts.skipped, 1);
assert.equal(result.summary.surface_count, 1);
assert.equal(result.summary.operation_count, 2);
assert.equal(result.summary.skipped_reason_codes.destructive_guardrail, 1);
assert.equal(result.summary.destructive_reason_codes.mutating_method, 1);
assert.equal(result.summary.role_boundary_outcomes.by_outcome.denied_as_expected, 1);
assert.equal(result.summary.db_query_metrics.query_count, 3);
assert.equal(result.summary.admin_browser_errors.errors, 1);
assert.equal(result.summary.http_guardrail_outcomes.blocked, 1);
assert.equal(result.provenance.workload_manifest, 'fuzz-manifest.json');

const passingBudgetResult = normalizeWordPressFuzzResult({
	schema: WORDPRESS_FUZZ_RESULT_SCHEMA,
	id: 'passing-budget-result',
	cases: [
		{
			id: 'within-budget',
			status: 'passed',
			duration_ms: 40,
			db_query: { query_count: 2 },
			admin_browser: { resource_count: 8 },
			memory: { peak_bytes: 1024 },
			budget: {
				max_duration_ms: 50,
				max_query_count: 3,
				max_memory_peak_bytes: 2048,
				max_browser_resource_count: 10,
			},
		},
	],
});

assert.equal(passingBudgetResult.status, 'passed');
assert.equal(passingBudgetResult.summary.budget_failure_count, 0);
assert.equal(passingBudgetResult.findings.length, 0);
assert.equal(passingBudgetResult.cases[0].performance_metrics.request_duration_ms, 40);
assert.equal(passingBudgetResult.cases[0].budget.max_request_duration_ms, 50);

const failingBudgetResult = normalizeWordPressFuzzResult({
	schema: WORDPRESS_FUZZ_RESULT_SCHEMA,
	id: 'failing-budget-result',
	budgets: { max_query_count: 4 },
	cases: [
		{
			id: 'slow-case',
			status: 'passed',
			durationMs: 75,
			dbQuery: { queryCount: 5 },
			adminBrowser: { resources: { count: 12 } },
			memory: { peakBytes: 4096 },
			budgets: {
				maxRequestDurationMs: 50,
				maxQueryCount: 3,
				maxMemoryPeakBytes: 2048,
				maxResourceCount: 10,
			},
		},
	],
});

assert.equal(failingBudgetResult.status, 'failed');
assert.equal(failingBudgetResult.cases[0].status, 'failed');
assert.equal(failingBudgetResult.summary.budget_failure_count, 5);
assert.deepEqual(failingBudgetResult.findings.map((finding) => finding.code), [
	'request_duration_budget_exceeded',
	'query_count_budget_exceeded',
	'memory_peak_budget_exceeded',
	'browser_resource_count_budget_exceeded',
	'query_count_budget_exceeded',
]);
assert.equal(failingBudgetResult.diagnostics[0].severity, 'failure');
assert.equal(failingBudgetResult.summary.performance_metrics.query_count, 5);

const performanceEvidenceResult = normalizeWordPressFuzzResult({
	schema: WORDPRESS_FUZZ_RESULT_SCHEMA,
	id: 'performance-evidence-result',
	cases: [
		{
			id: 'profiled-rest-case',
			status: 'passed',
			durationMs: 120,
			dbQuery: {
				queryCount: 7,
				durationMs: 18,
				topQueryShapes: [{ shape: 'SELECT * FROM wp_posts WHERE ID = ?', count: 4 }],
				topTables: [{ table: 'wp_posts', count: 5 }],
			},
			memory: { peakBytes: 4096 },
			browserMetrics: {
				browser_resource_count: 11,
				browser_request_count: 9,
				browser_failed_request_count: 1,
				browser_network_idle_ms: 250,
			},
			budget: {
				max_query_time_ms: 10,
				max_browser_failed_request_count: 0,
			},
		},
		{
			id: 'missing-metrics-case',
			status: 'passed',
		},
		{
			id: 'unsupported-metric-case',
			status: 'passed',
			metrics: { queryCount: 'many' },
		},
		{
			id: 'runtime-workload-case',
			status: 'passed',
			metadata: {
				execution: {
					result: {
						json: {
							executions: [
								{
									result: {
										json: {
											metrics: {
												query_count: 2,
												query_time_ms: 6,
												top_queries: [{ shape: 'SELECT option_value FROM wp_options WHERE option_name = ?', count: 2 }],
											},
										},
									},
								},
							],
						},
					},
				},
			},
		},
	],
});

assert.equal(performanceEvidenceResult.status, 'failed');
assert.equal(performanceEvidenceResult.cases[0].status, 'failed');
assert.equal(performanceEvidenceResult.cases[0].performance_metrics.request_duration_ms, 120);
assert.equal(performanceEvidenceResult.cases[0].performance_metrics.query_count, 7);
assert.equal(performanceEvidenceResult.cases[0].performance_metrics.query_time_ms, 18);
assert.equal(performanceEvidenceResult.cases[0].performance_metrics.memory_peak_bytes, 4096);
assert.equal(performanceEvidenceResult.cases[0].performance_metrics.browser_request_count, 9);
assert.equal(performanceEvidenceResult.cases[0].performance_metrics.browser_failed_request_count, 1);
assert.equal(performanceEvidenceResult.cases[0].performance_metrics.browser_network_idle_ms, 250);
assert.equal(performanceEvidenceResult.cases[0].performance_metric_reasons.query_time_ms.status, 'observed');
assert.equal(performanceEvidenceResult.cases[1].performance_metric_reasons.query_count.reason, 'metric_not_provided');
assert.equal(performanceEvidenceResult.cases[2].performance_metric_reasons.query_count.status, 'unsupported');
assert.equal(performanceEvidenceResult.cases[2].performance_metric_reasons.query_count.source_key, 'queryCount');
assert.equal(performanceEvidenceResult.cases[3].performance_metrics.query_count, 2);
assert.equal(performanceEvidenceResult.cases[3].performance_metrics.query_time_ms, 6);
assert.deepEqual(performanceEvidenceResult.cases[3].performance_summaries.top_queries, [{ shape: 'SELECT option_value FROM wp_options WHERE option_name = ?', count: 2 }]);
assert.deepEqual(performanceEvidenceResult.cases[0].performance_summaries.top_queries, [{ shape: 'SELECT * FROM wp_posts WHERE ID = ?', count: 4 }]);
assert.deepEqual(performanceEvidenceResult.cases[0].performance_summaries.top_tables, [{ table: 'wp_posts', count: 5 }]);
assert.equal(performanceEvidenceResult.cases[0].performance_summaries.browser_network.failed_request_count, 1);
assert.equal(performanceEvidenceResult.summary.budget_failure_count, 2);
assert.deepEqual(performanceEvidenceResult.findings.map((finding) => finding.code), [
	'query_time_budget_exceeded',
	'browser_failed_request_count_budget_exceeded',
]);
assert.equal(performanceEvidenceResult.summary.performance_metrics.request_duration_ms, 120);
assert.equal(performanceEvidenceResult.summary.performance_metrics.query_count, 9);
assert.equal(performanceEvidenceResult.summary.performance_metrics.query_time_ms, 24);
assert.equal(performanceEvidenceResult.summary.performance_metrics.memory_peak_bytes, 4096);
assert.equal(performanceEvidenceResult.summary.performance_metrics.browser_request_count, 9);
assert.equal(performanceEvidenceResult.summary.performance_metrics.browser_failed_request_count, 1);
assert.equal(performanceEvidenceResult.summary.performance_metrics.browser_network_idle_ms, 250);
assert.equal(performanceEvidenceResult.summary.performance_metric_reasons.query_count.observed, 2);
assert.equal(performanceEvidenceResult.summary.performance_metric_reasons.query_count.missing, 1);
assert.equal(performanceEvidenceResult.summary.performance_metric_reasons.query_count.unsupported, 1);

const errorStatusResult = normalizeWordPressFuzzResult({
	schema: WORDPRESS_FUZZ_RESULT_SCHEMA,
	status: 'error',
	cases: [{ id: 'error-case', status: 'error' }],
});

assert.equal(errorStatusResult.status, 'errored');
assert.equal(errorStatusResult.cases[0].status, 'errored');

assert.throws(() => normalizeWordPressSurfaceDiscovery({ schema: 'other/v1' }), /Unsupported/);
assert.throws(() => normalizeWordPressSurfaceDiscovery({ surfaces: [{ type: 'woocommerce-product' }] }), /Unsupported WordPress surface type/);
assert.throws(() => normalizeWordPressFuzzResult({ cases: [{ status: 'unknown' }] }), /Unsupported WordPress fuzz case status/);

console.log('wordpress fuzz schemas smoke passed');
