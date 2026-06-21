'use strict';

const assert = require('node:assert/strict');
const {
	WORDPRESS_SURFACE_DISCOVERY_SCHEMA,
	WORDPRESS_FUZZ_PLAN_SCHEMA,
	WORDPRESS_FUZZ_RESULT_SCHEMA,
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
	],
});

assert.equal(discovery.schema, 'wordpress-surface-discovery/v1');
assert.equal(discovery.surfaces[0].id, 'wp-v2-posts');
assert.equal(discovery.surfaces[1].id, 'admin-page-2');

const plan = normalizeWordPressFuzzPlan({
	schema: WORDPRESS_FUZZ_PLAN_SCHEMA,
	id: 'rest-route-fuzz',
	discovery_id: discovery.id,
	targets: [
		{
			id: 'posts-list-query',
			surface_id: 'wp-v2-posts',
			method: 'GET',
			route: '/wp/v2/posts',
			cases: [
				{ id: 'per-page-max', query: { per_page: 100 } },
				{ query: { search: '<script>' } },
			],
		},
	],
	budget: { max_cases: 25 },
});

assert.equal(plan.schema, 'wordpress-fuzz-plan/v1');
assert.equal(plan.discovery_id, 'site-surfaces');
assert.equal(plan.targets[0].cases[1].id, 'case-2');

const result = normalizeWordPressFuzzResult({
	schema: WORDPRESS_FUZZ_RESULT_SCHEMA,
	id: 'rest-route-fuzz-result',
	plan_id: plan.id,
	cases: [
		{ id: 'per-page-max', target_id: 'posts-list-query', status: 'passed', duration_ms: 42 },
		{ id: 'case-2', target_id: 'posts-list-query', status: 'failed', error: { message: '500 response' } },
	],
});

assert.equal(result.schema, 'wordpress-fuzz-result/v1');
assert.equal(result.status, 'failed');
assert.deepEqual(result.summary, { total: 2, passed: 1, failed: 1, errored: 0, skipped: 0 });

assert.throws(() => normalizeWordPressSurfaceDiscovery({ schema: 'other/v1' }), /Unsupported/);
assert.throws(() => normalizeWordPressSurfaceDiscovery({ surfaces: [{ type: 'woocommerce-product' }] }), /Unsupported WordPress surface type/);
assert.throws(() => normalizeWordPressFuzzResult({ cases: [{ status: 'unknown' }] }), /Unsupported WordPress fuzz case status/);

console.log('wordpress fuzz schemas smoke passed');
