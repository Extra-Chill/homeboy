'use strict';

/**
 * External dependencies
 */
const assert = require('node:assert/strict');

/**
 * Internal dependencies
 */
const {
	WORDPRESS_HOOK_FUZZ_PLAN_SCHEMA,
	WORDPRESS_HOOK_SURFACE_DISCOVERY_SCHEMA,
	createWordPressHookFuzzPlan,
	discoverWordPressHookSurfaces,
} = require('../lib/wordpress-hook-surface-discovery');

const discovery = discoverWordPressHookSurfaces({
	files: [
		{
			path: 'plugin.php',
			content: `<?php
add_action( 'init', 'example_zero_arg_init', 10, 0 );
add_action( 'save_post', 'example_save_post', 10, 3 );
add_filter( 'the_content', 'example_filter_content', 10, 1 );
add_action( $dynamic_hook, 'example_dynamic' );
wp_schedule_event( time(), 'hourly', 'example_hourly_cron' );
wp_schedule_single_event( time(), 'example_single_cron' );
`,
		},
	],
});

assert.equal(discovery.schema, WORDPRESS_HOOK_SURFACE_DISCOVERY_SCHEMA);
assert.equal(discovery.summary.file_count, 1);
assert.equal(discovery.summary.surface_count, 5);
assert.equal(discovery.summary.skipped_count, 1);

const init = discovery.surfaces.find((surface) => surface.hook === 'init');
assert.equal(init.type, 'action');
assert.equal(init.kind, 'hook_registration');
assert.equal(init.metadata.accepted_args, 0);
assert.deepEqual(init.invocation, {
	mode: 'do_action',
	hook: 'init',
	args: [],
	safe_to_auto_invoke: true,
	side_effect_risk: 'unknown',
});

const savePost = discovery.surfaces.find((surface) => surface.hook === 'save_post');
assert.equal(savePost.invocation.safe_to_auto_invoke, false);
assert.equal(savePost.invocation.skip_reason, 'requires_arguments');

const filter = discovery.surfaces.find((surface) => surface.hook === 'the_content');
assert.equal(filter.type, 'filter');
assert.equal(filter.invocation.safe_to_auto_invoke, false);
assert.equal(filter.invocation.skip_reason, 'requires_arguments');

const cron = discovery.surfaces.find((surface) => surface.hook === 'example_hourly_cron');
assert.equal(cron.type, 'cron');
assert.equal(cron.kind, 'cron_schedule');
assert.equal(cron.metadata.recurrence, 'hourly');
assert.equal(cron.invocation.safe_to_auto_invoke, false);
assert.equal(cron.invocation.skip_reason, 'cron_event_requires_runtime_schedule');

assert.equal(discovery.skipped[0].skip_reason, 'dynamic_hook_name');

const plan = createWordPressHookFuzzPlan({ discovery });
assert.equal(plan.schema, WORDPRESS_HOOK_FUZZ_PLAN_SCHEMA);
assert.equal(plan.summary.surface_count, 5);
assert.equal(plan.summary.case_count, 1);
assert.equal(plan.summary.skipped_count, 5);
assert.deepEqual(plan.cases.map((entry) => entry.hook), ['init']);
assert.equal(plan.cases[0].invocation.mode, 'do_action');
assert(plan.skipped.some((entry) => entry.hook === 'save_post' && entry.skip_reason === 'requires_arguments'));
assert(plan.skipped.some((entry) => entry.hook === 'example_hourly_cron' && entry.skip_reason === 'cron_event_requires_runtime_schedule'));
assert(plan.skipped.some((entry) => entry.skip_reason === 'dynamic_hook_name'));

const zeroArgFilterDiscovery = discoverWordPressHookSurfaces({
	allowZeroArgFilters: true,
	files: [{ path: 'filters.php', content: "<?php add_filter( 'example_zero_filter', 'callback', 10, 0 );" }],
});
assert.equal(zeroArgFilterDiscovery.surfaces[0].invocation.safe_to_auto_invoke, true);
assert.equal(zeroArgFilterDiscovery.surfaces[0].invocation.mode, 'apply_filters');

console.log('wordpress hook surface discovery smoke passed');
