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

assert.throws(() => normalizeWordPressSurfaceDiscovery({ schema: 'other/v1' }), /Unsupported/);
assert.throws(() => normalizeWordPressSurfaceDiscovery({ surfaces: [{ type: 'woocommerce-product' }] }), /Unsupported WordPress surface type/);
assert.throws(() => normalizeWordPressFuzzResult({ cases: [{ status: 'unknown' }] }), /Unsupported WordPress fuzz case status/);

console.log('wordpress fuzz schemas smoke passed');
