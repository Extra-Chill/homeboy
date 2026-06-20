'use strict';

/**
 * External dependencies
 */
const assert = require('node:assert/strict');

/**
 * Internal dependencies
 */
const {
	buildRestRouteMatrixArtifact,
	classifyRestRoute,
	formatRestRouteMatrixMarkdownReport,
	normalizeRestDbProfile,
	normalizeRestRouteMatrixBudgetManifest,
	normalizeWordPressRestRouteMatrix,
	restRouteMatrixKey,
	resolveRestRouteMatrixBudgets,
	summarizeRouteArgs,
	summarizeSchema,
} = require('../lib/rest-route-matrix');

const restIndex = {
	namespaces: ['wp/v2', 'wc/store/v1', 'demo/v1'],
	routes: {
		'/wc/store/v1/cart': {
			namespace: 'wc/store/v1',
			methods: ['GET', 'POST'],
			endpoints: [
				{ methods: ['GET'], args: { context: { type: 'string', enum: ['view', 'edit'], default: 'view' } } },
				{ methods: ['POST'], args: { items: { type: 'array', required: true } } },
			],
			schema: {
				type: 'object',
				properties: {
					items: { type: 'array' },
					totals: { type: 'object' },
				},
			},
		},
		'/wp/v2/posts': {
			namespace: 'wp/v2',
			methods: ['GET', 'POST'],
			endpoints: [
				{
					methods: ['GET'],
					args: {
						context: { type: 'string', enum: ['view', 'embed', 'edit'], default: 'view' },
						page: { type: 'integer', default: 1 },
						per_page: { type: 'integer', default: 10 },
						search: { type: 'string' },
					},
				},
			],
			schema: {
				type: 'object',
				properties: {
					id: { type: 'integer' },
					link: { type: 'string' },
					title: { type: 'object' },
				},
			},
		},
		'/wp/v2/posts/(?P<id>[\\d]+)': {
			namespace: 'wp/v2',
			methods: ['GET', 'PUT', 'PATCH', 'DELETE'],
			endpoints: [
				{ methods: ['GET'], args: { id: { type: 'integer', required: true }, context: { type: 'string' } } },
			],
		},
		'/demo/v1/private': {
			namespace: 'demo/v1',
			methods: ['GET', 'OPTIONS'],
			endpoints: [{ methods: ['GET'], args: {} }],
		},
	},
};

assert.equal(restRouteMatrixKey({ method: 'get', route: '/wp/v2/posts/(?P<id>[\\d]+)' }), 'rest:get:wp-v2-posts-id');
assert.deepEqual(classifyRestRoute('/wp/v2/posts/(?P<id>[\\d]+)', { namespace: 'wp/v2' }), {
	namespace: 'wp/v2',
	family: 'wordpress-core',
	kind: 'item',
	segments: ['posts', '(?P<id>[\\d]+)'],
	hasPathParams: true,
});

const coreGetCases = normalizeWordPressRestRouteMatrix(restIndex, {
	includeNamespaces: ['wp/v2'],
	methods: ['GET'],
	maxArgs: 2,
	maxSchemaProperties: 2,
});

assert.deepEqual(coreGetCases.map((entry) => entry.id), [
	'rest:get:wp-v2-posts',
	'rest:get:wp-v2-posts-id',
]);
assert.equal(coreGetCases[0].path, '/wp/v2/posts');
assert.equal(coreGetCases[0].classification.kind, 'collection');
assert.equal(coreGetCases[0].argsSummary.count, 4);
assert.equal(coreGetCases[0].argsSummary.truncated, true);
assert.deepEqual(coreGetCases[0].argsSummary.args.map((arg) => arg.name), ['context', 'page']);
assert.equal(coreGetCases[0].schemaSummary.propertyCount, 3);
assert.equal(coreGetCases[0].schemaSummary.truncated, true);
assert.deepEqual(coreGetCases[0].schemaSummary.properties.map((property) => property.name), ['id', 'link']);
assert.equal(coreGetCases[1].path, '/wp/v2/posts/{id}');
assert.equal(coreGetCases[1].classification.hasPathParams, true);

const allDiscoveredCases = normalizeWordPressRestRouteMatrix(restIndex);
assert.equal(allDiscoveredCases.length, 10);
assert.equal(allDiscoveredCases.some((entry) => entry.id === 'rest:options:demo-v1-private'), true);

const nonCorePostCases = normalizeWordPressRestRouteMatrix(restIndex.routes, {
	excludeNamespaces: ['wp/v2', 'demo/v1'],
	method: 'POST',
});

assert.equal(nonCorePostCases.length, 1);
assert.equal(nonCorePostCases[0].id, 'rest:post:wc-store-v1-cart');
assert.equal(nonCorePostCases[0].namespace, 'wc/store/v1');
assert.equal(nonCorePostCases[0].classification.family, 'extension');

const argSummary = summarizeRouteArgs(restIndex.routes['/wc/store/v1/cart']);
assert.equal(argSummary.count, 2);
assert.deepEqual(argSummary.args.map((arg) => arg.name), ['context', 'items']);

const schemaSummary = summarizeSchema(restIndex.routes['/wc/store/v1/cart'].schema, { maxSchemaProperties: 1 });
assert.equal(schemaSummary.propertyCount, 2);
assert.deepEqual(schemaSummary.properties, [{ name: 'items', type: 'array' }]);

const artifact = buildRestRouteMatrixArtifact({
	routes: restIndex,
	caseResults: [
		{ method: 'GET', route: '/wp/v2/posts', status: 200, durationMs: 40, queryCount: 3, authenticated: false },
		{ method: 'GET', route: '/wc/store/v1/cart', status: 200, durationMs: 95 },
		{ method: 'POST', route: '/wc/store/v1/cart', status: 500, durationMs: 80, queryCount: 2, authenticated: true },
	],
	dbProfiles: [
		{ method: 'GET', route: '/wc/store/v1/cart', queryCount: 14, queryTimeMs: 22.5, totalQueries: 40, top_query_shapes: [
			{ sql: 'SELECT * FROM wp_posts WHERE ID = ?', count: 3, time_ms: 12.25 },
			{ sql: 'SELECT * FROM wp_postmeta WHERE post_id IN (?)', count: 7, time_ms: 8.5 },
		] },
		{ method: 'GET', route: '/demo/v1/private', query_count: 1, query_time_ms: 1.25 },
	],
	budgets: {
		maxDurationMs: 90,
		maxQueryCount: 10,
		allowedStatuses: [200, 201],
	},
}, {
	methods: ['GET', 'POST'],
	slowestLimit: 5,
});

assert.equal(artifact.schema, 'homeboy/wordpress-rest-route-matrix-artifact/v1');
assert.equal(artifact.totals.routeCount, 6);
assert.equal(artifact.totals.resultCount, 3);
assert.equal(artifact.totals.dbProfileCount, 2);
assert.equal(artifact.totals.coveredCount, 4);
assert.equal(artifact.totals.uncoveredCount, 2);
assert.equal(artifact.routes.find((row) => row.id === 'rest:get:wc-store-v1-cart').queryCount, 14);
assert.equal(artifact.routes.find((row) => row.id === 'rest:get:wc-store-v1-cart').queryTimeMs, 22.5);
assert.deepEqual(artifact.routes.find((row) => row.id === 'rest:get:wc-store-v1-cart').dbProfile, {
	queryCount: 14,
	queryTimeMs: 22.5,
	totalQueries: 40,
	topQueryShapes: [
		{ sql: 'SELECT * FROM wp_posts WHERE ID = ?', count: 3, timeMs: 12.25 },
		{ sql: 'SELECT * FROM wp_postmeta WHERE post_id IN (?)', count: 7, timeMs: 8.5 },
	],
});
assert.equal(artifact.coverage.byNamespace['wc/store/v1'].total, 2);
assert.equal(artifact.coverage.byNamespace['wc/store/v1'].covered, 2);
assert.equal(artifact.coverage.byMethod.GET.total, 4);
assert.equal(artifact.coverage.byMethod.GET.uncovered, 1);
assert.equal(artifact.coverage.byStatus['200'].covered, 2);
assert.equal(artifact.coverage.byStatus['500'].covered, 1);
assert.equal(artifact.coverage.byAuth.anonymous.covered, 1);
assert.equal(artifact.coverage.byAuth.authenticated.covered, 1);
assert.equal(artifact.coverage.byRoute['/wc/store/v1/cart'].covered, 2);
assert.deepEqual(artifact.coverage.byRouteDetails['/wc/store/v1/cart'], {
	total: 2,
	covered: 2,
	uncovered: 0,
	methods: {
		GET: { total: 1, covered: 1, uncovered: 0 },
		POST: { total: 1, covered: 1, uncovered: 0 },
	},
	statuses: {
		200: { total: 1, covered: 1, uncovered: 0 },
		500: { total: 1, covered: 1, uncovered: 0 },
	},
	auth: {
		authenticated: { total: 1, covered: 1, uncovered: 0 },
		unknown: { total: 1, covered: 1, uncovered: 0 },
	},
});
assert.deepEqual(artifact.coverageGaps.map((gap) => gap.id), [
	'rest:get:wp-v2-posts-id',
	'rest:post:wp-v2-posts',
]);
assert.equal(artifact.topQueryShapesByRoute[0].id, 'rest:get:wc-store-v1-cart');
assert.deepEqual(artifact.slowestByDuration.map((row) => row.id), [
	'rest:get:wc-store-v1-cart',
	'rest:post:wc-store-v1-cart',
	'rest:get:wp-v2-posts',
]);
assert.deepEqual(artifact.slowestByQueryCount.map((row) => row.id), [
	'rest:get:wc-store-v1-cart',
	'rest:get:wp-v2-posts',
	'rest:post:wc-store-v1-cart',
	'rest:get:demo-v1-private',
]);
assert.deepEqual(artifact.slowestByQueryTime.map((row) => row.id), [
	'rest:get:wc-store-v1-cart',
	'rest:get:demo-v1-private',
]);
assert.deepEqual(artifact.missingRoutes.map((row) => row.id), [
	'rest:get:wp-v2-posts-id',
	'rest:post:wp-v2-posts',
]);
assert.deepEqual(artifact.budgetFindings.map((finding) => finding.type), [
	'duration_budget_exceeded',
	'query_count_budget_exceeded',
	'status_not_allowed',
]);

const markdown = formatRestRouteMatrixMarkdownReport(artifact, { limit: 5 });
assert.match(markdown, /## WordPress REST route matrix/);
assert.match(markdown, /Routes: 6; results: 3; covered: 4; uncovered: 2; budget findings: 3/);
assert.match(markdown, /## Coverage by namespace/);
assert.match(markdown, /\| wc\/store\/v1 \| 2 \| 2 \| 0 \|/);
assert.match(markdown, /## Coverage by auth/);
assert.match(markdown, /## Slowest routes by duration/);
assert.match(markdown, /`GET \/wc\/store\/v1\/cart` \| 200 \| 95 \| 14/);
assert.match(markdown, /## Slowest routes by query time/);
assert.match(markdown, /## Top DB query shapes by REST case/);
assert.match(markdown, /SELECT \* FROM wp_posts WHERE ID = \?/);
assert.match(markdown, /## Coverage gaps/);
assert.match(markdown, /## Missing or uncovered routes/);
assert.match(markdown, /`GET \/wp\/v2\/posts\/\{id\}`/);
assert.match(markdown, /## Budget findings/);

assert.deepEqual(normalizeRestDbProfile({ method: 'get', route: '/wp-json/wp/v2/posts', query_time_ms: 3.5 }), {
	id: 'rest:get:wp-v2-posts',
	key: 'rest:get:wp-v2-posts',
	method: 'GET',
	path: '/wp/v2/posts',
	route: '/wp/v2/posts',
	status: undefined,
	durationMs: undefined,
	queryCount: undefined,
	queryTimeMs: 3.5,
	totalQueries: undefined,
	authCoverage: 'unknown',
	topQueryShapes: [],
});

const budgetManifest = normalizeRestRouteMatrixBudgetManifest({
	defaults: { maxDurationMs: 200, maxQueryCount: 30, allowedStatuses: [200] },
	namespaces: [{ namespace: 'wc/store/v1', maxQueryCount: 12 }],
	methods: [{ method: 'POST', allowedStatuses: [200, 201, 204] }],
	routes: [{ id: 'rest:get:wc-store-v1-cart', maxDurationMs: 90, maxQueryCount: 8 }],
});

assert.equal(budgetManifest.schema, 'homeboy/wordpress-rest-route-matrix-budgets/v1');
assert.deepEqual(budgetManifest.defaults, { maxDurationMs: 200, maxQueryCount: 30, allowedStatuses: [200] });
assert.deepEqual(resolveRestRouteMatrixBudgets({
	id: 'rest:get:wc-store-v1-cart',
	method: 'GET',
	route: '/wc/store/v1/cart',
	namespace: 'wc/store/v1',
}, budgetManifest), { maxDurationMs: 90, maxQueryCount: 8, allowedStatuses: [200] });
assert.deepEqual(resolveRestRouteMatrixBudgets({
	id: 'rest:post:wc-store-v1-cart',
	method: 'POST',
	route: '/wc/store/v1/cart',
	namespace: 'wc/store/v1',
}, budgetManifest), { maxDurationMs: 200, maxQueryCount: 12, allowedStatuses: [200, 201, 204] });

const budgetedArtifact = buildRestRouteMatrixArtifact({
	routes: restIndex,
	caseResults: [
		{ method: 'GET', route: '/wc/store/v1/cart', status: 200, durationMs: 95, queryCount: 9 },
		{ method: 'POST', route: '/wc/store/v1/cart', status: 500, durationMs: 80, queryCount: 13 },
	],
	budgets: budgetManifest,
}, {
	methods: ['GET', 'POST'],
});

assert.equal(budgetedArtifact.budgets.schema, 'homeboy/wordpress-rest-route-matrix-budgets/v1');
assert.deepEqual(budgetedArtifact.budgetFindings.map((finding) => `${finding.id}:${finding.type}`), [
	'rest:get:wc-store-v1-cart:duration_budget_exceeded',
	'rest:get:wc-store-v1-cart:query_count_budget_exceeded',
	'rest:post:wc-store-v1-cart:query_count_budget_exceeded',
	'rest:post:wc-store-v1-cart:status_not_allowed',
]);

console.log('REST route matrix smoke passed.');
