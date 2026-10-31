'use strict';

const assert = require('node:assert/strict');

const {
	WORDPRESS_ADMIN_FUZZ_SURFACE_DISCOVERY_SCHEMA,
	WORDPRESS_ADMIN_FUZZ_SURFACE_SCHEMA,
	classifyWordPressAdminPageFuzzSurface,
	discoverWordPressAdminPageFuzzSurfaces,
	normalizeWordPressAdminPageFuzzSurface,
} = require('../lib/admin-page-fuzz-surfaces');

assert.equal(classifyWordPressAdminPageFuzzSurface({ path: '/wp-admin/index.php' }), 'dashboard');
assert.equal(classifyWordPressAdminPageFuzzSurface({ path: '/wp-admin/edit.php?post_type=page' }), 'list_table');
assert.equal(classifyWordPressAdminPageFuzzSurface({ path: '/wp-admin/site-editor.php?path=%2Fpatterns' }), 'site_editor');
assert.equal(classifyWordPressAdminPageFuzzSurface({ path: '/wp-admin/admin.php?page=generic' }), 'registered_admin_page');

const readySurface = normalizeWordPressAdminPageFuzzSurface({
	id: 'generic-settings',
	label: 'Generic settings',
	path: '/wp-admin/options-general.php',
	ready: { selector: '#wpbody-content' },
});
assert.equal(readySurface.schema, WORDPRESS_ADMIN_FUZZ_SURFACE_SCHEMA);
assert.equal(readySurface.surfaceKind, 'settings');
assert.equal(readySurface.status, 'ready');
assert.deepEqual(readySurface.skipReasons, []);
assert.deepEqual(readySurface.unsafeReasons, []);

const interactionSurface = normalizeWordPressAdminPageFuzzSurface({
	id: 'bulk-posts',
	path: '/wp-admin/edit.php',
	forms: [{ id: 'bulk-action', method: 'POST', nonce_action: 'bulk-posts', capability: 'edit_posts' }],
	actions: [{ id: 'view-filter', method: 'GET', selector: '#filter-by-date' }],
}, { includeUnsafe: true });
assert.equal(interactionSurface.forms.length, 1);
assert.equal(interactionSurface.actions.length, 1);

const unsafeSurface = normalizeWordPressAdminPageFuzzSurface({
	id: 'delete-item',
	path: '/wp-admin/edit.php?action=delete&post=1',
});
assert.equal(unsafeSurface.status, 'unsafe');
assert.deepEqual(unsafeSurface.unsafeReasons, ['mutating_action']);

const skippedSurface = normalizeWordPressAdminPageFuzzSurface({
	id: 'template-route',
	path: '/wp-admin/site-editor.php?postId={templateSlug}',
	params: ['templateSlug'],
});
assert.equal(skippedSurface.status, 'skipped');
assert.deepEqual(skippedSurface.skipReasons, ['missing_params']);
assert.deepEqual(skippedSurface.metadata.missingParams, ['templateSlug']);

const discovery = discoverWordPressAdminPageFuzzSurfaces({
	surfaces: [
		{ id: 'dashboard', path: '/wp-admin/index.php' },
		{ id: 'external', path: 'https://example.com/wp-login.php' },
		{ id: 'needs-param', path: '/wp-admin/admin.php?page={page}', params: ['page'] },
	],
});
assert.equal(discovery.schema, WORDPRESS_ADMIN_FUZZ_SURFACE_DISCOVERY_SCHEMA);
assert.equal(discovery.totals.total, 3);
assert.equal(discovery.totals.ready, 1);
assert.equal(discovery.totals.unsafe, 1);
assert.equal(discovery.totals.skipped, 1);
assert.equal(discovery.totals.byKind.dashboard, 1);
assert.equal(discovery.totals.byKind.registered_admin_page, 1);

console.log('WordPress admin page fuzz surfaces smoke passed.');
