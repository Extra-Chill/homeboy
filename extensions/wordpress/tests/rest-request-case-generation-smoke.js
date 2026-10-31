'use strict';

/**
 * External dependencies
 */
const assert = require('node:assert/strict');

/**
 * Internal dependencies
 */
const {
	buildWordPressRestRequestCasesArtifact,
	generateWordPressRestRequestCases,
	generateWordPressRestRequestCasesForEntry,
	normalizeWordPressRestRouteInventory,
	normalizeWordPressRestRouteMatrix,
} = require('../lib/rest-route-matrix');

const restIndex = {
	routes: {
		'/example/v1/items': {
			namespace: 'example/v1',
			methods: ['GET', 'POST'],
			endpoints: [
				{
					methods: ['GET'],
					args: {
						context: { type: 'string', enum: ['view', 'embed'], default: 'view' },
						page: { type: 'integer', default: 1, minimum: 1 },
						per_page: { type: 'integer', default: 10, minimum: 1, maximum: 100 },
						search: { type: 'string' },
					},
				},
				{
					methods: ['POST'],
					args: {
						name: { type: 'string', required: true },
						status: { type: 'string', enum: ['draft', 'publish'], default: 'draft' },
					},
				},
			],
		},
		'/example/v1/items/(?P<id>[\\d]+)': {
			namespace: 'example/v1',
			methods: ['GET', 'HEAD', 'OPTIONS'],
			endpoints: [
				{
					methods: ['GET'],
					args: {
						id: { type: 'integer', required: true },
						context: { type: 'string', enum: ['view', 'edit'], default: 'view' },
					},
				},
			],
		},
		'/example/v1/search/(?P<term>[^/]+)': {
			namespace: 'example/v1',
			methods: ['GET'],
			endpoints: [
				{
					methods: ['GET'],
					args: {},
				},
			],
		},
	},
};

const getCases = generateWordPressRestRequestCases(restIndex, {
	methods: ['GET'],
	seed: 'alpha',
	maxCases: 20,
});

assert.deepEqual(getCases.map((requestCase) => requestCase.key), [
	'rest:get:example-v1-items:baseline',
	'rest:get:example-v1-items:default:context',
	'rest:get:example-v1-items:enum:context:view',
	'rest:get:example-v1-items:enum:context:embed',
	'rest:get:example-v1-items:pagination:page',
	'rest:get:example-v1-items:default:page',
	'rest:get:example-v1-items:pagination:per-page',
	'rest:get:example-v1-items:default:per-page',
	'rest:get:example-v1-items-id:baseline',
	'rest:get:example-v1-items-id:default:context',
	'rest:get:example-v1-items-id:enum:context:view',
	'rest:get:example-v1-items-id:enum:context:edit',
]);

const pageCase = getCases.find((requestCase) => requestCase.key === 'rest:get:example-v1-items:pagination:page');
assert.deepEqual(pageCase.request, {
	method: 'GET',
	path: '/example/v1/items',
	query: { page: 2 },
});

const itemBaseline = getCases.find((requestCase) => requestCase.matrixId === 'rest:get:example-v1-items-id');
assert.equal(itemBaseline.request.path, '/example/v1/items/1');

const safeDefaultCases = generateWordPressRestRequestCases(restIndex, { seed: 'alpha', maxCases: 30 });
const safeDefaultMethods = new Set(safeDefaultCases.map((requestCase) => requestCase.method));
assert.deepEqual([...safeDefaultMethods].sort(), ['GET', 'HEAD', 'OPTIONS']);
assert.equal(safeDefaultCases.some((requestCase) => requestCase.method === 'POST'), false);
assert.deepEqual(safeDefaultCases.find((requestCase) => requestCase.method === 'HEAD').request, {
	method: 'HEAD',
	path: '/example/v1/items/1',
});
assert.deepEqual(safeDefaultCases.find((requestCase) => requestCase.method === 'OPTIONS').expectedStatuses, [200, 204]);

const plannedKinds = new Set(getCases[0].metadata.plannedCases.map((requestCase) => requestCase.kind));
assert.equal(plannedKinds.has('invalid-enum'), true);
assert.equal(plannedKinds.has('numeric-boundary'), true);

const postMatrixEntry = normalizeWordPressRestRouteMatrix(restIndex, { methods: ['POST'] })[0];
const postCases = generateWordPressRestRequestCasesForEntry(
	postMatrixEntry,
	restIndex.routes['/example/v1/items'],
	{ seed: 'alpha', maxCases: 10 }
);

const requiredCase = postCases.find((requestCase) => requestCase.variant === 'required-args');
assert.deepEqual(requiredCase.request, {
	method: 'POST',
	path: '/example/v1/items',
	body: { name: 'test' },
});
assert.equal(postCases.some((requestCase) => requestCase.metadata.plannedCases.some((planned) => planned.kind === 'missing-required-arg')), true);

const limitedCases = generateWordPressRestRequestCases(restIndex, { methods: ['GET'], seed: 'alpha', maxCases: 3 });
assert.deepEqual(limitedCases.map((requestCase) => requestCase.key), getCases.slice(0, 3).map((requestCase) => requestCase.key));

const inventory = normalizeWordPressRestRouteInventory(restIndex, { generatedAt: '2026-01-01T00:00:00.000Z' });
assert.equal(inventory.schema, 'homeboy/wordpress-rest-route-inventory/v1');
assert.equal(inventory.totals.routes, 3);
assert.equal(inventory.routes.some((route) => route.id === 'rest:post:example-v1-items'), true);

const casesArtifact = buildWordPressRestRequestCasesArtifact(inventory, {
	seed: 'alpha',
	maxCases: 50,
	generatedAt: '2026-01-01T00:00:00.000Z',
});
assert.equal(casesArtifact.schema, 'homeboy/wordpress-rest-request-cases/v1');
assert.deepEqual(casesArtifact.filters.methods, ['GET', 'HEAD', 'OPTIONS']);
assert.equal(casesArtifact.cases.some((requestCase) => requestCase.method === 'POST'), false);
assert.equal(casesArtifact.skippedRoutes.some((route) => route.id === 'rest:post:example-v1-items' && route.type === 'unsafe_method'), true);
assert.equal(casesArtifact.skippedRoutes.some((route) => route.id === 'rest:get:example-v1-search-term' && route.type === 'unsupported_parameterized_route'), true);
assert.deepEqual(casesArtifact.routeGaps.map((gap) => gap.id), ['rest:get:example-v1-search-term']);
assert.deepEqual(casesArtifact.cases[0].auth, { context: 'anonymous', role: null });

const unsafeArtifact = buildWordPressRestRequestCasesArtifact(restIndex, { methods: ['POST'], seed: 'alpha', maxCases: 10 });
assert.equal(unsafeArtifact.cases.length, 0);
assert.equal(unsafeArtifact.skippedRoutes.some((route) => route.id === 'rest:post:example-v1-items' && route.type === 'unsafe_method'), true);

console.log('REST request case generation smoke passed.');
