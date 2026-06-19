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
	normalizeWordPressRestRouteMatrix,
	restRouteMatrixKey,
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
			methods: ['GET'],
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
		{ method: 'GET', route: '/wp/v2/posts', status: 200, durationMs: 40, queryCount: 3 },
		{ method: 'GET', route: '/wc/store/v1/cart', status: 200, durationMs: 95, queryCount: 14 },
		{ method: 'POST', route: '/wc/store/v1/cart', status: 500, durationMs: 80, queryCount: 2 },
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
assert.equal(artifact.totals.coveredCount, 3);
assert.equal(artifact.totals.uncoveredCount, 3);
assert.equal(artifact.coverage.byNamespace['wc/store/v1'].total, 2);
assert.equal(artifact.coverage.byNamespace['wc/store/v1'].covered, 2);
assert.equal(artifact.coverage.byMethod.GET.total, 4);
assert.equal(artifact.coverage.byMethod.GET.uncovered, 2);
assert.equal(artifact.coverage.byStatus['200'].covered, 2);
assert.equal(artifact.coverage.byStatus['500'].covered, 1);
assert.deepEqual(artifact.slowestByDuration.map((row) => row.id), [
	'rest:get:wc-store-v1-cart',
	'rest:post:wc-store-v1-cart',
	'rest:get:wp-v2-posts',
]);
assert.deepEqual(artifact.slowestByQueryCount.map((row) => row.id), [
	'rest:get:wc-store-v1-cart',
	'rest:get:wp-v2-posts',
	'rest:post:wc-store-v1-cart',
]);
assert.deepEqual(artifact.missingRoutes.map((row) => row.id), [
	'rest:get:demo-v1-private',
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
assert.match(markdown, /Routes: 6; results: 3; covered: 3; uncovered: 3; budget findings: 3/);
assert.match(markdown, /## Coverage by namespace/);
assert.match(markdown, /\| wc\/store\/v1 \| 2 \| 2 \| 0 \|/);
assert.match(markdown, /## Slowest routes by duration/);
assert.match(markdown, /`GET \/wc\/store\/v1\/cart` \| 200 \| 95 \| 14/);
assert.match(markdown, /## Missing or uncovered routes/);
assert.match(markdown, /`GET \/demo\/v1\/private`/);
assert.match(markdown, /## Budget findings/);

console.log('REST route matrix smoke passed.');
