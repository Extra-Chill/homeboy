'use strict';

/**
 * External dependencies
 */
const assert = require('node:assert/strict');

/**
 * Internal dependencies
 */
const {
	WORDPRESS_REST_ROUTE_DISCOVERY_SCHEMA,
	discoverWordPressRestRoutes,
	normalizeWordPressRestRouteDiscovery,
	routeDiscoveryUrl,
} = require('../lib/wordpress-rest-route-discovery');

const restIndex = {
	routes: {
		'/wp/v2/posts': {
			namespace: 'wp/v2',
			methods: ['GET'],
			endpoints: [{ methods: ['GET'], args: { page: { type: 'integer', default: 1 } } }],
		},
		'/demo/v1/items': {
			namespace: 'demo/v1',
			methods: ['GET', 'POST'],
			endpoints: [
				{ methods: ['GET'], args: { context: { type: 'string', enum: ['view', 'edit'] } }, permission_callback: '__return_true' },
				{ methods: ['POST'], args: { name: { type: 'string', required: true } }, permission_callback: 'demo_can_create_item' },
			],
		},
	},
};

const discovery = normalizeWordPressRestRouteDiscovery({
	restIndex,
	optionsByRoute: {
		'/demo/v1/items': {
			endpoints: [
				{ methods: ['GET'], args: { context: { type: 'string', enum: ['view', 'edit'] }, per_page: { type: 'integer', maximum: 100 } }, permission_callback: '__return_true' },
				{ methods: ['POST'], args: { name: { type: 'string', required: true }, status: { type: 'string', default: 'draft' } }, permission_callback: 'demo_can_create_item' },
			],
		},
	},
	schemasByRoute: {
		'/demo/v1/items': { schema: { type: 'object', properties: { id: { type: 'integer' }, name: { type: 'string' } } } },
	},
}, { generatedAt: '2026-01-01T00:00:00.000Z' });

assert.equal(discovery.schema, WORDPRESS_REST_ROUTE_DISCOVERY_SCHEMA);
assert.equal(discovery.totals.routes, 2);
assert.equal(discovery.totals.entries, 3);
assert.deepEqual(discovery.routes.map((route) => route.id), [
	'rest:get:demo-v1-items',
	'rest:post:demo-v1-items',
	'rest:get:wp-v2-posts',
]);

const postRoute = discovery.routes.find((route) => route.id === 'rest:post:demo-v1-items');
assert.equal(postRoute.auth.required, true);
assert.equal(postRoute.auth.callback, 'demo_can_create_item');
assert.deepEqual(Object.keys(postRoute.args).sort(), ['name', 'status']);
assert.equal(postRoute.schemaSummary.propertyCount, 2);
assert.deepEqual(postRoute.sources, ['rest-index', 'options', 'schema']);

const getRoute = discovery.routes.find((route) => route.id === 'rest:get:demo-v1-items');
assert.equal(getRoute.auth.required, false);
assert.equal(getRoute.argsSummary.count, 2);

assert.equal(routeDiscoveryUrl('https://example.test/site', '/wp/v2/posts', 'context=help'), 'https://example.test/site/wp-json/wp/v2/posts?context=help');

(async () => {
	const calls = [];
	const fetchedDiscovery = await discoverWordPressRestRoutes({ baseUrl: 'https://example.test' }, {
		generatedAt: '2026-01-01T00:00:00.000Z',
		fetch: async (url, request = {}) => {
			calls.push({ url, method: request.method || 'GET' });
			if (url === 'https://example.test/wp-json/') {
				return restIndex;
			}
			if (request.method === 'OPTIONS') {
				return { endpoints: [{ methods: ['GET'], args: { page: { type: 'integer' } }, permission_callback: '__return_true' }] };
			}
			return { schema: { type: 'object', properties: { id: { type: 'integer' } } } };
		},
	});

	assert.equal(fetchedDiscovery.routes.length, 3);
	assert.deepEqual(calls.map((call) => call.method), ['GET', 'OPTIONS', 'GET', 'OPTIONS', 'GET']);
	console.log('WordPress REST route discovery smoke passed.');
})().catch((error) => {
	process.stderr.write(`${error.stack || error.message}\n`);
	process.exit(1);
});
