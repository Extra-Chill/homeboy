'use strict';

const assert = require('node:assert/strict');

const {
	WORDPRESS_ADMIN_FORM_ACTION_SURFACE_DISCOVERY_SCHEMA,
	normalizeWordPressAdminFormActionSurfaceDiscovery,
} = require('../lib/wordpress-admin-form-action-surfaces');
const {
	buildWordPressFuzzPlanFromSurfaces,
} = require('../lib/wordpress-fuzz-plan-from-surfaces');

function actionContract(action) {
	return {
		schema: 'wp-codebox/wordpress-runtime-action/v1',
		action,
		ability: `wp-codebox/runtime-action/${action}`,
	};
}

const codeboxRuntimeContracts = {
	schema: 'wp-codebox/wordpress-runtime-action-contracts/v1',
	schemas: {
		wordpressRuntime: {
			adminAction: 'wp-codebox/wordpress-admin-action/v1',
			adminPost: 'wp-codebox/wordpress-admin-post/v1',
			ajaxAction: 'wp-codebox/wordpress-ajax-action/v1',
			nonce: 'wp-codebox/wordpress-nonce/v1',
		},
	},
	actions: Object.fromEntries(['admin_page_load', 'admin_action', 'admin_post', 'ajax_action', 'nonce'].map((action) => [action, actionContract(action)])),
};

const descriptorFixture = {
	schema: 'wp-codebox/admin-form-action-descriptors/v1-fixture',
	id: 'fixture-admin-surfaces',
	admin_pages: [{
		id: 'tools-page',
		title: 'Tools page',
		menu_slug: 'generic-tools',
		path: '/wp-admin/admin.php?page=generic-tools',
		capability: 'manage_options',
		forms: [{
			id: 'save-settings',
			method: 'post',
			action_path: '/wp-admin/admin-post.php',
			admin_post_action: 'generic_save_settings',
			nonce: { field: '_wpnonce', action: 'generic_save_settings' },
			inputs: [{ name: 'setting_name', type: 'text', required: true }],
			submit_controls: [{ name: 'submit', value: 'save', label: 'Save settings' }],
			capability: 'manage_options',
		}, {
			id: 'preview-settings',
			method: 'post',
			action_path: '/wp-admin/admin-ajax.php',
			ajax_action: 'generic_preview_settings',
			nonce_action: 'generic_preview_settings',
			inputs: [{ name: 'preview_value', type: 'text' }],
		}],
		list_table: {
			capability: 'delete_posts',
			bulk_actions: [{ value: 'delete', label: 'Delete selected', destructive: true, nonce: { action: 'bulk-generic-items' } }],
		},
	}],
	ajax_actions: [{
		id: 'ajax:refresh-cache',
		action: 'generic_refresh_cache',
		method: 'POST',
		nonce: { action: 'generic_refresh_cache' },
		inputs: [{ name: 'cache_key', type: 'text' }],
	}],
	admin_post_actions: [{
		id: 'admin-post:purge-cache',
		action: 'generic_purge_cache',
		method: 'POST',
		destructive: true,
		nonce: { action: 'generic_purge_cache' },
	}],
};

const discovery = normalizeWordPressAdminFormActionSurfaceDiscovery(descriptorFixture);
assert.equal(discovery.schema, WORDPRESS_ADMIN_FORM_ACTION_SURFACE_DISCOVERY_SCHEMA);
assert.equal(discovery.id, 'fixture-admin-surfaces');
assert.equal(discovery.surfaces.length, 3);
assert.deepEqual(discovery.totals.by_type, { 'admin-page': 2, 'ajax-action': 1 });
assert.equal(discovery.totals.destructive, 1);
assert.equal(discovery.metadata.source_schema, 'wp-codebox/admin-form-action-descriptors/v1-fixture');

const adminPage = discovery.surfaces.find((surface) => surface.id === 'tools-page');
assert.equal(adminPage.forms.length, 2);
assert.equal(adminPage.forms[0].method, 'POST');
assert.equal(adminPage.forms[0].action_path, '/wp-admin/admin-post.php');
assert.deepEqual(adminPage.forms[0].nonce_context, { required: true, action: 'generic_save_settings', field: '_wpnonce' });
assert.deepEqual(adminPage.forms[0].input_descriptors, [{ name: 'setting_name', type: 'text', required: true }]);
assert.deepEqual(adminPage.forms[0].submit_controls, [{ name: 'submit', value: 'save', label: 'Save settings', destructive: false }]);
assert.equal(adminPage.actions[0].bulk_action.value, 'delete');
assert.deepEqual(adminPage.actions[0].safety.reason_codes, ['declared_destructive', 'destructive_action_term']);

const ajaxSurface = discovery.surfaces.find((surface) => surface.type === 'ajax-action');
assert.equal(ajaxSurface.path, '/wp-admin/admin-ajax.php');
assert.equal(ajaxSurface.action, 'generic_refresh_cache');

const adminPostSurface = discovery.surfaces.find((surface) => surface.id === 'admin-post:purge-cache');
assert.equal(adminPostSurface.path, '/wp-admin/admin-post.php');
assert.deepEqual(adminPostSurface.destructive_reasons, ['declared_destructive']);

const plan = buildWordPressFuzzPlanFromSurfaces(discovery, { codeboxRuntimeContracts });
const toolsTarget = plan.targets.find((target) => target.id === 'tools-page');
const formCases = Object.fromEntries(toolsTarget.cases.map((testCase) => [testCase.operation.interaction_id, testCase]));

assert.equal(formCases['save-settings'].runtime_operation.action, 'admin_post');
assert.equal(formCases['save-settings'].runtime_operation.input.path, '/wp-admin/admin-post.php');
assert.equal(formCases['save-settings'].runtime_operation.input.action, 'generic_save_settings');
assert.deepEqual(formCases['save-settings'].runtime_operation.input.nonce_context, { required: true, action: 'generic_save_settings', field: '_wpnonce' });
assert.deepEqual(formCases['save-settings'].runtime_operation.input.input_descriptors, [{ name: 'setting_name', type: 'text', required: true }]);

assert.equal(formCases['preview-settings'].runtime_operation.action, 'ajax_action');
assert.equal(formCases['preview-settings'].runtime_operation.input.path, '/wp-admin/admin-ajax.php');
assert.equal(formCases.delete.runtime_operation.action, 'admin_action');
assert.equal(formCases.delete.runtime_operation.input.bulk_action.value, 'delete');
assert.deepEqual(formCases.delete.destructive_reasons, ['declared_destructive', 'destructive_action_term']);

const standaloneAjaxCase = plan.targets.find((target) => target.id === 'ajax:refresh-cache').cases[0];
assert.equal(standaloneAjaxCase.runtime_operation.action, 'ajax_action');
assert.equal(standaloneAjaxCase.runtime_operation.input.action, 'generic_refresh_cache');
assert.deepEqual(standaloneAjaxCase.runtime_operation.input.input_descriptors, [{ name: 'cache_key', type: 'text', required: false }]);

const standaloneAdminPostCase = plan.targets.find((target) => target.id === 'admin-post:purge-cache').cases[0];
assert.equal(standaloneAdminPostCase.runtime_operation.action, 'admin_post');
assert.equal(standaloneAdminPostCase.runtime_operation.input.action, 'generic_purge_cache');
assert.deepEqual(standaloneAdminPostCase.destructive_reasons, ['declared_destructive']);

assert(!JSON.stringify(discovery).includes('woocommerce'));
assert(!JSON.stringify(plan).includes('woocommerce'));

console.log('WordPress admin form/action surfaces smoke passed.');
