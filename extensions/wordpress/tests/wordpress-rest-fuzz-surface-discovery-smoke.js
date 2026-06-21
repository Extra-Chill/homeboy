'use strict';

/**
 * External dependencies
 */
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

/**
 * Internal dependencies
 */
const {
	WORDPRESS_FUZZ_SURFACES_SCHEMA,
	discoverWordPressRestFuzzSurfaces,
} = require('../lib/wordpress-rest-fuzz-surface-discovery');

const restIndex = {
	routes: {
		'/wp/v2/posts': {
			namespace: 'wp/v2',
			methods: ['GET', 'POST'],
			endpoints: [{ methods: ['GET'], args: { page: { type: 'integer' } } }],
			schema: { type: 'object', properties: { id: { type: 'integer' } } },
		},
		'/demo/v1/items/(?P<id>[\\d]+)': {
			namespace: 'demo/v1',
			methods: ['GET'],
			endpoints: [{ methods: ['GET'], args: { id: { type: 'integer', required: true } } }],
		},
	},
};

const fallback = discoverWordPressRestFuzzSurfaces({ restIndex }, {
	methods: ['GET'],
	generatedAt: '2026-01-01T00:00:00.000Z',
});

assert.equal(fallback.schema, 'homeboy/wordpress-rest-fuzz-surface-discovery/v1');
assert.equal(fallback.source, 'minimal-fallback');
assert.equal(fallback.artifact.schema, WORDPRESS_FUZZ_SURFACES_SCHEMA);
assert.equal(fallback.artifact.surfaces[0].kind, 'rest');
assert.deepEqual(fallback.artifact.surfaces[0].methods, ['GET']);
assert.deepEqual(fallback.artifact.surfaces[0].namespaces, ['demo/v1', 'wp/v2']);
assert.deepEqual(fallback.artifact.surfaces[0].routes.map((route) => route.id), [
	'rest:get:demo-v1-items-id',
	'rest:get:wp-v2-posts',
]);
assert.equal(fallback.artifact.surfaces[0].routes[1].args.count, 1);

const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'wordpress-rest-fuzz-surfaces-'));
fs.mkdirSync(path.join(repoRoot, '.homeboy'));
const sharedSchema = {
	schema: WORDPRESS_FUZZ_SURFACES_SCHEMA,
	type: 'wordpress-fuzz-surfaces',
	surfaces: [{ id: 'repo-owned-rest', kind: 'rest', routes: [] }],
};
fs.writeFileSync(path.join(repoRoot, '.homeboy', 'wordpress-fuzz-surfaces.json'), JSON.stringify(sharedSchema));

const repository = discoverWordPressRestFuzzSurfaces({}, { repoRoot });
assert.equal(repository.source, 'repository');
assert.deepEqual(repository.artifact, sharedSchema);
assert.match(repository.source_path, /wordpress-fuzz-surfaces\.json$/);

const inline = discoverWordPressRestFuzzSurfaces({ surfaceSchema: sharedSchema });
assert.equal(inline.source, 'inline');
assert.equal(inline.artifact, sharedSchema);

console.log('WordPress REST fuzz surface discovery smoke passed.');
