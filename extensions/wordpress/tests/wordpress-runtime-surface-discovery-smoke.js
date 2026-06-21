'use strict';

const assert = require('node:assert/strict');

const {
	buildWordPressRuntimeSurfaceCoverageManifest,
	normalizeWordPressRuntimeSurfaceDiscovery,
} = require('../lib/wordpress-runtime-surface-discovery');

const discovery = normalizeWordPressRuntimeSurfaceDiscovery({
	artifacts: [
		{
			schema: 'homeboy/wordpress-rest-fuzz-surface-discovery/v1',
			artifact: {
				schema: 'homeboy/wordpress-fuzz-surfaces/v1',
				source: 'rest-index',
				surfaces: [{
					kind: 'rest',
					routes: [
						{ route: '/wp/v2/posts', method: 'GET' },
						{ route: '/wp/v2/posts', method: 'POST' },
					],
				}],
			},
		},
		{
			schema: 'homeboy/wordpress-admin-page-fuzz-surface-discovery/v1',
			surfaces: [{ path: '/wp-admin/tools.php', label: 'Tools' }],
		},
		{
			schema: 'homeboy/wordpress-ajax-action-surface/v1',
			actions: [{ action: 'heartbeat' }],
		},
		{
			schema: 'homeboy/wordpress-db-inventory/v1',
			tables: [{ name: 'wp_posts' }],
		},
		{
			schema: 'homeboy/wordpress-discovery-inventory/v1',
			surfaces: [{ id: 'front:home', type: 'frontend-url', path: '/' }],
			blocks: [{ name: 'example/card', title: 'Card' }],
		},
	],
});

assert.equal(discovery.schema, 'homeboy/wordpress-surface-discovery/v1');
assert.deepEqual(discovery.surfaces.map((surface) => surface.id), [
	'admin:/wp-admin/tools.php',
	'ajax:heartbeat',
	'block:example/card',
	'db:wp_posts',
	'frontend:/',
	'rest:/wp/v2/posts',
]);
assert.equal(discovery.surfaces.find((surface) => surface.id === 'rest:/wp/v2/posts').metadata.sources.includes('rest-index'), true);

const manifest = buildWordPressRuntimeSurfaceCoverageManifest(discovery);
assert.equal(manifest.schema, 'homeboy/wordpress-fuzz-coverage-manifest/v1');
assert.deepEqual(manifest.surfaces.map((surface) => surface.type), [
	'admin_page',
	'ajax_action',
	'block',
	'db_table',
	'frontend_url',
	'rest_route',
]);

console.log('WordPress runtime surface discovery smoke passed.');
