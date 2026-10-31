'use strict';

const assert = require('node:assert/strict');

const {
	buildWordPressLiveSurfaceDiscoveryArtifact,
	runWordPressLiveSurfaceDiscoveryWorkload,
} = require('../lib/wordpress-live-surface-discovery');

const artifact = buildWordPressLiveSurfaceDiscoveryArtifact({
	generated_at: '2026-06-23T00:00:00.000Z',
	restRoutes: [{ route: '/wp/v2/posts', methods: ['GET'], source: 'rest_get_server' }],
	adminPages: [{ path: '/wp-admin/tools.php', name: 'Tools', source: 'admin_menu' }],
	databaseTables: [{ name: 'wp_posts', source: 'wpdb' }],
	frontendUrls: [{ url: 'https://example.test/', label: 'Home', source: 'home_url' }],
	blocks: [{ name: 'core/paragraph', title: 'Paragraph', source: 'WP_Block_Type_Registry' }],
	ajaxActions: [{ action: 'heartbeat', source: 'wp_filter' }],
	cronEvents: [{ event: 'wp_version_check', source: '_get_cron_array' }],
	cronSchedules: [{ event: 'hourly', interval: 3600, source: 'wp_get_schedules' }],
	postTypes: [{ name: 'post', label: 'Posts', source: 'get_post_types' }],
	taxonomies: [{ name: 'category', label: 'Categories', source: 'get_taxonomies' }],
	roles: [{ role: 'administrator', label: 'Administrator', capabilityCount: 10, source: 'wp_roles' }],
	capabilities: [{ capability: 'edit_posts', roleCount: 1, source: 'wp_roles' }],
	users: [{ user: 'all', label: 'All users', count: 2, source: 'count_users' }],
	options: [{ option: 'blogname', autoload: 'yes', source: 'wpdb_options_metadata' }],
	settings: [{ setting: 'blogname', settingType: 'string', group: 'general', source: 'wp_registered_settings' }],
	media: [{ media: 'image/jpeg', count: 3, source: 'wp_count_attachments' }],
	wpCliCommands: [{ command: 'plugin list', source: 'WP_CLI_command_registry' }],
	unsupported: [{ type: 'block', reason: 'ignored_when_supported' }],
	supportedTypes: ['block'],
});

assert.equal(artifact.schema, 'homeboy/wordpress-surface-discovery/v1');
assert.deepEqual(artifact.surfaces.map((surface) => surface.id), [
	'admin:/wp-admin/tools.php',
	'ajax:heartbeat',
	'block:core/paragraph',
	'db:wp_posts',
	'frontend:https://example.test/',
	'rest:/wp/v2/posts',
]);
assert.deepEqual(artifact.unsupported_surfaces.map((surface) => surface.id), [
	'capability:edit_posts',
	'cron:hourly',
	'cron:wp_version_check',
	'media:image/jpeg',
	'option:blogname',
	'post-type:post',
	'role:administrator',
	'setting:blogname',
	'taxonomy:category',
	'user:all',
	'wp-cli:plugin list',
]);
assert.equal(artifact.diagnostics.length, 11);
assert.deepEqual(artifact.metadata.unsupported_surfaces, []);

const unsupportedArtifact = buildWordPressLiveSurfaceDiscoveryArtifact({
	unsupported: [
		{ type: 'rest_route', reason: 'missing_rest_server', message: 'rest_get_server() is unavailable.' },
		{ type: 'admin_page', reason: 'admin_menu_failed', message: 'admin_menu hook failed.' },
	],
});

assert.equal(unsupportedArtifact.surfaces.length, 0);
assert.deepEqual(unsupportedArtifact.metadata.unsupported_surfaces.map((row) => row.type), ['admin_page', 'rest_route']);
assert.equal(unsupportedArtifact.metadata.unsupported_surfaces[0].supported, false);

(async () => {
	const workload = await runWordPressLiveSurfaceDiscoveryWorkload({
		collector: async () => ({
			frontendUrls: [{ url: 'https://example.test/about/', label: 'About' }],
			unsupported: ['db_table'],
		}),
	});

	assert.equal(workload.artifact.schema, 'homeboy/wordpress-surface-discovery/v1');
	assert.equal(workload.metrics.wordpress_surface_frontend_url_count, 1);
	assert.equal(workload.metrics.wordpress_surface_unsupported_count, 1);
	assert.equal(workload.metadata.unsupported_surface_count, 1);
	console.log('WordPress live surface discovery smoke passed.');
})().catch((error) => {
	console.error(error);
	process.exit(1);
});
