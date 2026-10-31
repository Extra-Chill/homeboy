'use strict';

/**
 * External dependencies
 */
const assert = require('node:assert/strict');

/**
 * Internal dependencies
 */
const {
	ajaxActionPlanKey,
	buildAjaxActionPlanArtifact,
	classifyAjaxAction,
	formatAjaxActionPlanMarkdownReport,
	normalizeWordPressAjaxActionSurface,
	parseAjaxHookName,
} = require('../lib/ajax-action-surface');

assert.deepEqual(parseAjaxHookName('wp_ajax_get_status'), {
	hook: 'wp_ajax_get_status',
	audience: 'authenticated',
	action: 'get_status',
});
assert.deepEqual(parseAjaxHookName('wp_ajax_nopriv_search_items'), {
	hook: 'wp_ajax_nopriv_search_items',
	audience: 'anonymous',
	action: 'search_items',
});
assert.equal(parseAjaxHookName('init'), null);
assert.equal(ajaxActionPlanKey('get_status'), 'ajax:get-status');
assert.equal(classifyAjaxAction('save_settings').intent, 'mutation');

const surface = normalizeWordPressAjaxActionSurface({
	wp_ajax_get_status: { callback: 'Plugin\\Status::ajax', file: 'includes/status.php' },
	wp_ajax_save_settings: 'save_settings_callback',
	wp_ajax_search_items: 'search_items_callback',
	wp_ajax_nopriv_search_items: 'search_items_callback',
	wp_ajax_export_tokens: 'export_tokens_callback',
	init: 'ignored',
});

assert.equal(surface.schema, 'homeboy/wordpress-ajax-action-surface/v1');
assert.equal(surface.totals.actionCount, 4);
assert.equal(surface.totals.hookCount, 5);
assert.equal(surface.totals.authenticatedCount, 4);
assert.equal(surface.totals.anonymousCount, 1);
assert.equal(surface.actions.find((action) => action.action === 'get_status').safety.level, 'low');
assert.equal(surface.actions.find((action) => action.action === 'save_settings').safety.intent, 'mutation');
assert.equal(surface.actions.find((action) => action.action === 'search_items').safety.level, 'medium');
assert.deepEqual(surface.actions.find((action) => action.action === 'search_items').plan.skipReasons, [
	'unauthenticated_ajax_requires_explicit_opt_in',
]);
assert.deepEqual(surface.actions.find((action) => action.action === 'save_settings').plan.skipReasons, [
	'mutating_action_requires_explicit_opt_in',
]);
assert.deepEqual(surface.actions.find((action) => action.action === 'export_tokens').plan.skipReasons, [
	'sensitive_action_requires_explicit_opt_in',
]);

const plan = buildAjaxActionPlanArtifact(surface);
assert.equal(plan.schema, 'homeboy/wordpress-ajax-action-plan/v1');
assert.equal(plan.totals.plannedCount, 1);
assert.equal(plan.plannedActions[0].action, 'get_status');
assert.equal(plan.plannedActions[0].path, '/wp-admin/admin-ajax.php');
assert.deepEqual(plan.plannedActions[0].body, { action: 'get_status' });
assert.equal(plan.skippedActions.length, 3);

const permissivePlan = buildAjaxActionPlanArtifact(surface, {
	includeUnauthenticated: true,
	includeMutating: true,
	includeSensitive: true,
});
assert.equal(permissivePlan.totals.plannedCount, 4);

const markdown = formatAjaxActionPlanMarkdownReport(plan);
assert.match(markdown, /## WordPress AJAX action surface plan/);
assert.match(markdown, /Actions: 4; planned: 1; skipped: 3/);
assert.match(markdown, /`get_status`/);
assert.match(markdown, /mutating_action_requires_explicit_opt_in/);

console.log('ajax action surface smoke passed');
