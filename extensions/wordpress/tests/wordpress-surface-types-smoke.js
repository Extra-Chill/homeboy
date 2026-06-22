'use strict';

const assert = require('node:assert/strict');

const {
	WORDPRESS_RUNTIME_SURFACE_ID_PREFIXES,
	WORDPRESS_SURFACE_COLLECTION_KEYS,
	WORDPRESS_SURFACE_TYPES,
	normalizeWordPressRuntimeSurfaceType,
	normalizeWordPressSurfaceType,
	wordpressSurfaceTypeFromCollectionKey,
} = require('../lib/wordpress-surface-types');
const { normalizeWordPressDiscoverySurface } = require('../lib/wordpress-discovery-inventory');
const { normalizeWordPressSurfaceDiscovery } = require('../lib/wordpress-fuzz-schemas');
const { normalizeWordPressRuntimeSurfaceDiscovery } = require('../lib/wordpress-runtime-surface-discovery');
const { buildWordPressFuzzPlanFromSurfaces } = require('../lib/wordpress-fuzz-plan-from-surfaces');

assert.deepEqual(WORDPRESS_SURFACE_TYPES.slice(0, 4), ['admin-page', 'ajax-action', 'block', 'capability']);
assert.equal(WORDPRESS_SURFACE_COLLECTION_KEYS.includes('surfaces'), true);
assert.equal(WORDPRESS_SURFACE_COLLECTION_KEYS.includes('restRoutes'), true);
assert.equal(WORDPRESS_RUNTIME_SURFACE_ID_PREFIXES.ajax_action, 'ajax');

assert.equal(normalizeWordPressSurfaceType('admin_page'), 'admin-page');
assert.equal(normalizeWordPressSurfaceType('database_table'), 'database-table');
assert.equal(normalizeWordPressSurfaceType('db_table'), 'database-table');
assert.equal(normalizeWordPressSurfaceType('database_query'), 'db-query');
assert.equal(normalizeWordPressSurfaceType('external_http'), 'external-http');
assert.equal(normalizeWordPressSurfaceType('rest_route'), 'rest-route');
assert.equal(normalizeWordPressSurfaceType('ajax'), 'ajax-action');
assert.equal(normalizeWordPressSurfaceType('mystery_surface'), '');
assert.equal(normalizeWordPressSurfaceType('mystery_surface', { allowUnknown: true }), 'mystery_surface');

assert.equal(normalizeWordPressRuntimeSurfaceType('admin-page'), 'admin_page');
assert.equal(normalizeWordPressRuntimeSurfaceType('database-table'), 'db_table');
assert.equal(normalizeWordPressRuntimeSurfaceType('db_table'), 'db_table');
assert.equal(normalizeWordPressRuntimeSurfaceType('frontend-url'), 'frontend_url');
assert.equal(normalizeWordPressRuntimeSurfaceType('rest-route'), 'rest_route');
assert.equal(normalizeWordPressRuntimeSurfaceType('ajax-action'), 'ajax_action');
assert.equal(normalizeWordPressRuntimeSurfaceType('hook'), '');

assert.equal(wordpressSurfaceTypeFromCollectionKey('databaseTables'), 'database-table');
assert.equal(wordpressSurfaceTypeFromCollectionKey('db_queries'), 'db-query');
assert.equal(wordpressSurfaceTypeFromCollectionKey('admin_pages'), 'admin-page');

assert.equal(normalizeWordPressDiscoverySurface({ kind: 'admin_page', path: '/wp-admin/' }).type, 'admin-page');
assert.equal(normalizeWordPressDiscoverySurface({ kind: 'ajax_action', action: 'heartbeat' }).type, 'ajax-action');

const schemaDiscovery = normalizeWordPressSurfaceDiscovery({
	surfaces: [
		{ kind: 'db_table', id: 'db:posts' },
		{ kind: 'database_query', id: 'query:posts' },
		{ kind: 'ajax_action', id: 'ajax:heartbeat' },
	],
});
assert.deepEqual(schemaDiscovery.surfaces.map((surface) => surface.type), ['database-table', 'db-query', 'ajax-action']);

const runtimeDiscovery = normalizeWordPressRuntimeSurfaceDiscovery({
	surfaces: [
		{ kind: 'admin-page', path: '/wp-admin/edit.php' },
		{ kind: 'ajax-action', action: 'heartbeat' },
		{ kind: 'database-table', table: 'wp_posts' },
	],
});
assert.deepEqual(runtimeDiscovery.surfaces.map((surface) => surface.type), ['admin_page', 'ajax_action', 'db_table']);

const plan = buildWordPressFuzzPlanFromSurfaces({
	databaseTables: ['wp_posts'],
	db_queries: [{ id: 'query:posts', query: 'SELECT ID FROM wp_posts' }],
	admin_pages: [{ id: 'admin:posts', path: '/wp-admin/edit.php' }],
});
assert.deepEqual(plan.targets.map((target) => target.type), ['database-table', 'db-query', 'admin-page']);

console.log('WordPress surface types smoke passed.');
