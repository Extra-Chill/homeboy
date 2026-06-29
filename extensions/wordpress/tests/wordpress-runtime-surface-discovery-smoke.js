'use strict';

const assert = require('node:assert/strict');

const {
	buildWordPressRuntimeSurfaceCoverageManifest,
	normalizeWordPressRuntimeSurfaceDiscovery,
} = require('../lib/wordpress-runtime-surface-discovery');
const fixtureInventory = require('./fixtures/wordpress-surface-family-inventory.json');

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
		{
			options: [{ option: 'blogname', autoload: 'yes' }],
			settings: [{ setting: 'blogname', type: 'string' }],
			postTypes: [{ name: 'post', label: 'Posts' }],
			taxonomies: [{ name: 'category', label: 'Categories' }],
			roles: [{ role: 'administrator', capabilityCount: 10 }],
			capabilities: [{ capability: 'edit_posts', roleCount: 1 }],
			users: [{ user: 'all', count: 2 }],
			media: [{ media: 'image/jpeg', count: 3 }],
			wpCliCommands: [{ command: 'plugin list' }],
			cronEvents: [{ event: 'wp_version_check' }],
		},
	],
});

assert.equal(discovery.schema, 'homeboy/wordpress-surface-discovery/v1');
assert.deepEqual(discovery.surfaces.map((surface) => surface.id), [
	'admin:/wp-admin/tools.php',
	'ajax:heartbeat',
	'block:example/card',
	'crud:all',
	'crud:blogname',
	'crud:category',
	'crud:post',
	'db:wp_posts',
	'frontend:/',
	'rest:/wp/v2/posts',
]);
assert.equal(discovery.surfaces.find((surface) => surface.id === 'rest:/wp/v2/posts').metadata.sources.includes('rest-index'), true);
assert.equal(discovery.surfaces.every((surface) => surface.execution_tier === 'read_only_executable'), true);
assert.equal(discovery.surfaces.find((surface) => surface.id === 'crud:post').workload.action, 'crud_operation');
assert.equal(discovery.surfaces.find((surface) => surface.id === 'crud:post').workload.command, 'wordpress.crud-operation');
assert.equal(discovery.surfaces.find((surface) => surface.id === 'crud:post').workload.input.resource.kind, 'post');
assert.equal(discovery.surfaces.find((surface) => surface.id === 'block:example/card').workload.status, 'blocked');
assert.deepEqual(discovery.surfaces.find((surface) => surface.id === 'block:example/card').workload.blocker.missing_contract_fields, ['actions.block_render', 'actions.block_editor']);
assert.deepEqual(discovery.unsupported_surfaces.map((surface) => surface.id), [
	'capability:edit_posts',
	'cron:wp_version_check',
	'media:image/jpeg',
	'role:administrator',
	'setting:blogname',
	'wp-cli:plugin list',
]);
assert.equal(discovery.unsupported_surfaces.every((surface) => surface.execution_tier === 'discovered'), true);
assert.equal(discovery.diagnostics.length, 6);
assert.equal(discovery.diagnostics.find((diagnostic) => diagnostic.surface.type === 'wp_cli_command').code, 'wp_codebox_runtime_contract_missing');
assert.deepEqual(discovery.diagnostics.find((diagnostic) => diagnostic.surface.type === 'wp_cli_command').missing_contract_fields, ['actions.wp_cli', 'commands.wordpress.wp-cli']);

const manifest = buildWordPressRuntimeSurfaceCoverageManifest(discovery);
assert.equal(manifest.schema, 'homeboy/wordpress-fuzz-coverage-manifest/v1');
assert.deepEqual(manifest.surfaces.map((surface) => surface.type), [
	'admin_page',
	'ajax_action',
	'block',
	'crud_resource',
	'crud_resource',
	'crud_resource',
	'crud_resource',
	'db_table',
	'frontend_url',
	'rest_route',
]);
assert.equal(manifest.surfaces.every((surface) => surface.execution_tier === 'read_only_executable'), true);

const fixtureDiscovery = normalizeWordPressRuntimeSurfaceDiscovery(fixtureInventory);
assert.deepEqual(fixtureDiscovery.surfaces.map((surface) => surface.id), [
	'admin:posts',
	'ajax:heartbeat',
	'block:paragraph',
	'crud:blogname',
	'crud:category',
	'crud:post',
	'crud:subscriber',
	'db:posts',
	'frontend:/',
	'rest:wp-v2-posts',
]);
assert.equal(fixtureDiscovery.surfaces.find((surface) => surface.id === 'crud:category').workload.input.resource.kind, 'term');
assert.equal(fixtureDiscovery.surfaces.find((surface) => surface.id === 'frontend:/').workload.action, 'page');
assert.equal(fixtureDiscovery.surfaces.find((surface) => surface.id === 'frontend:/').workload.command, 'wordpress.frontend-page-load');
assert.equal(fixtureDiscovery.surfaces.find((surface) => surface.id === 'admin:posts').workload.action, 'admin_page');
assert.equal(fixtureDiscovery.surfaces.find((surface) => surface.id === 'admin:posts').workload.command, 'wordpress.admin-page-load');
assert.equal(fixtureDiscovery.surfaces.find((surface) => surface.id === 'rest:wp-v2-posts').workload.action, 'rest_request');
assert.equal(fixtureDiscovery.surfaces.find((surface) => surface.id === 'block:paragraph').workload.status, 'blocked');
assert.deepEqual(fixtureDiscovery.surfaces.find((surface) => surface.id === 'block:paragraph').workload.blocker.missing_contract_fields, ['actions.block_render', 'actions.block_editor']);
assert.equal(fixtureDiscovery.unsupported_surfaces.find((surface) => surface.type === 'db_query').blocker.code, 'wp_codebox_runtime_contract_missing');
assert.deepEqual(fixtureDiscovery.unsupported_surfaces.find((surface) => surface.type === 'db_query').blocker.missing_contract_fields, ['actions.db_query', 'commands.wordpress.db-query']);
assert.equal(fixtureDiscovery.unsupported_surfaces.find((surface) => surface.type === 'wp_cli_command').blocker.code, 'wp_codebox_runtime_contract_missing');

console.log('WordPress runtime surface discovery smoke passed.');
