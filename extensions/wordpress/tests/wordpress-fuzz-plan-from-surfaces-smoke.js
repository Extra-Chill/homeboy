'use strict';

const assert = require('node:assert/strict');

const {
	buildWordPressFuzzPlanFromSurfaces,
	collectWordPressFuzzPlanSurfaces,
} = require('../lib/wordpress-fuzz-plan-from-surfaces');
const {
	normalizeWordPressRuntimeSurfaceDiscovery,
} = require('../lib/wordpress-runtime-surface-discovery');

const manifest = {
	id: 'generic-core-surfaces',
	hooks: [{ id: 'hook:init', hook: 'init' }],
	cron_events: [{ id: 'cron:wp_version_check', event: 'wp_version_check' }],
	options: { blogname: { option: 'blogname' } },
	post_types: { post: { post_type: 'post' } },
	taxonomies: { category: { taxonomy: 'category' } },
	media: [{ id: 'media:attachment', name: 'attachment' }],
	users: [{ id: 'user:subscriber', name: 'subscriber' }],
	roles: [{ id: 'role:editor', role: 'editor', capability: 'edit_posts' }],
	capabilities: [{ id: 'cap:manage-options', capability: 'manage_options' }],
	database: {
		tables: { posts: { table: 'wp_posts' } },
		queries: { postsLookup: { query: 'SELECT ID FROM wp_posts WHERE post_type = ?' } },
	},
	external_http: { requests: [{ id: 'http:api-example', url: 'https://api.example.test/v1/', method: 'GET' }] },
	blocks: [{ id: 'block:core-paragraph', name: 'core/paragraph', block_name: 'core/paragraph' }],
	frontend: [{ id: 'front:home', path: '/' }],
	admin: [{ id: 'admin:posts', path: '/wp-admin/edit.php' }],
	rest: [{ id: 'rest:wp-v2-posts', method: 'GET', route: '/wp/v2/posts' }],
};

const surfaces = collectWordPressFuzzPlanSurfaces(manifest);
assert.equal(surfaces.length, 16);

const plan = buildWordPressFuzzPlanFromSurfaces(manifest, { seed: 'seed-1' });
assert.equal(plan.schema, 'wordpress-fuzz-plan/v1');
assert.equal(plan.discovery_id, 'generic-core-surfaces');
assert.equal(plan.targets.length, 16);

const targetTypes = Object.fromEntries(plan.targets.map((target) => [target.type, target]));
assert.equal(targetTypes.hook.cases[0].intent, 'exercise-hook');
assert.equal(targetTypes['cron-event'].cases[0].intent, 'inspect-cron-event');
assert.deepEqual(targetTypes.option.cases.map((testCase) => testCase.intent), ['read-option', 'create-option', 'update-option', 'delete-option']);
assert.equal(targetTypes.option.cases[0].operation.resource_type, 'option');
assert.equal(targetTypes.option.cases[0].operation.input.option, 'blogname');
assert.equal(targetTypes.option.cases[0].operation.safety.level, 'safe');
assert.equal(targetTypes.option.cases[1].operation.capability_context.required[0], 'manage_options');
assert.deepEqual(targetTypes.option.cases[1].skip_reasons, ['crud_mutation_requires_explicit_allow']);
assert.deepEqual(targetTypes['post-type'].cases.map((testCase) => testCase.intent), ['list-posts', 'read-post', 'create-post', 'update-post', 'delete-post']);
assert.equal(targetTypes['post-type'].cases[0].operation.resource_type, 'post');
assert.equal(targetTypes['post-type'].cases[0].operation.input.post_type, 'post');
assert.equal(targetTypes['post-type'].cases[3].operation.rollback_policy.strategy, 'restore-snapshot');
assert.equal(targetTypes.taxonomy.cases[0].operation.resource_type, 'term');
assert.equal(targetTypes.taxonomy.cases[0].operation.input.taxonomy, 'category');
assert.equal(targetTypes.media.cases[0].intent, 'query-media');
assert.deepEqual(targetTypes.user.cases.map((testCase) => testCase.intent), ['list-users', 'read-user', 'create-user', 'update-user', 'delete-user']);
assert.equal(targetTypes.user.cases[2].operation.capability_context.required[0], 'create_users');
assert.equal(targetTypes.role.cases[0].intent, 'check-role-boundary');
assert.equal(targetTypes.capability.cases[0].intent, 'check-capability-boundary');
assert.equal(targetTypes['database-table'].cases[0].intent, 'inspect-database-table');
assert.equal(targetTypes['db-query'].cases[0].intent, 'profile-database-query');
assert.equal(targetTypes['external-http'].cases[0].intent, 'exercise-external-http-guardrail');
assert.equal(targetTypes['db-query'].cases[0].operation.query, 'SELECT ID FROM wp_posts WHERE post_type = ?');
assert.equal(targetTypes['external-http'].cases[0].operation.url, 'https://api.example.test/v1/');
assert(targetTypes.role.operation_id.includes('check-role-boundary'));
assert.equal(targetTypes.block.cases[0].operation.block_name, 'core/paragraph');
assert.equal(targetTypes['frontend-url'].cases[0].operation.path, '/');
assert.equal(targetTypes['admin-page'].cases[0].operation.path, '/wp-admin/edit.php');
assert.equal(targetTypes['rest-route'].cases[0].operation.route, '/wp/v2/posts');
assert.equal(targetTypes['rest-route'].cases[0].seed, 'seed-1');
assert(!JSON.stringify(plan).includes('woocommerce'), 'fuzz plan conversion must stay product-agnostic');

const aliasPlan = buildWordPressFuzzPlanFromSurfaces({
	surfaces: [
		{ kind: 'rest', id: 'rest-alias', route: '/wp/v2/pages' },
		{ kind: 'frontend', id: 'frontend-alias', url: 'https://example.test/' },
		{ kind: 'users', id: 'users-alias' },
		{ kind: 'action', id: 'action-alias', hook: 'init' },
	],
});
assert.deepEqual(aliasPlan.targets.map((target) => target.type), ['rest-route', 'frontend-url', 'user', 'hook']);
assert.equal(aliasPlan.targets[0].cases.length, 1, 'REST routes without a declared resource keep legacy single-case behavior');

const runtimeDiscovery = normalizeWordPressRuntimeSurfaceDiscovery({
	id: 'runtime-surfaces',
	surfaces: [
		{ type: 'rest_route', route: '/wp/v2/posts', method: 'GET' },
		{ type: 'admin_page', path: '/wp-admin/tools.php' },
		{ type: 'ajax_action', action: 'heartbeat' },
		{ type: 'db_table', table: 'wp_posts' },
	],
});
const runtimePlan = buildWordPressFuzzPlanFromSurfaces(runtimeDiscovery);
assert.deepEqual(runtimePlan.targets.map((target) => target.type), ['admin-page', 'ajax-action', 'database-table', 'rest-route']);
assert.equal(runtimePlan.targets.find((target) => target.type === 'ajax-action').cases[0].intent, 'exercise-ajax-action');

const nestedPlan = buildWordPressFuzzPlanFromSurfaces({
	hooks: {
		actions: { init: { hook: 'init' } },
		filters: { the_content: { hook: 'the_content' } },
	},
	rest: {
		routes: { '/wp/v2/posts': { route: '/wp/v2/posts', method: 'GET' } },
	},
	postTypes: ['page'],
	frontendUrls: [{ id: 'front:sample', path: '/sample-page/' }],
});
assert.deepEqual(nestedPlan.targets.map((target) => target.type), ['hook', 'hook', 'post-type', 'frontend-url', 'rest-route']);

const legacyRestPlan = buildWordPressFuzzPlanFromSurfaces({
	rest: [{ id: 'rest:legacy', route: '/wp/v2/legacy' }],
});
assert.equal(legacyRestPlan.targets[0].cases.length, 1);
assert.equal(legacyRestPlan.targets[0].cases[0].id, 'rest:legacy-generic-fuzz');
assert.equal(legacyRestPlan.targets[0].cases[0].operation.method, undefined);

const methodAwareRestPlan = buildWordPressFuzzPlanFromSurfaces({
	rest: [{
		id: 'rest:wp-v2-posts',
		route: '/wp/v2/posts',
		methods: ['GET', 'POST', 'DELETE'],
		auth: { required: true, source: 'permission_callback', capability: 'edit_posts' },
		args: {
			count: 2,
			args: [
				{ name: 'search', type: 'string', required: false },
				{ name: 'status', type: 'string', required: true },
			],
		},
	}],
}, { seed: 'seed-rest' });
const restTarget = methodAwareRestPlan.targets[0];
assert.deepEqual(restTarget.cases.map((testCase) => testCase.operation.method), ['DELETE', 'GET', 'POST']);
const getCase = restTarget.cases.find((testCase) => testCase.operation.method === 'GET');
assert.equal(getCase.intent, 'request-rest-route');
assert.deepEqual(getCase.skip_reasons, []);
assert.deepEqual(getCase.destructive_reasons, []);
assert.equal(getCase.metadata.safety.mutates, false);
assert.equal(getCase.metadata.surface.args.count, 2);
assert.equal(getCase.metadata.surface.args.args[1].required, true);
assert.equal(getCase.seed, 'seed-rest');
for (const method of ['POST', 'DELETE']) {
	const testCase = restTarget.cases.find((entry) => entry.operation.method === method);
	assert(testCase.skip_reasons.includes('mutating_rest_method_requires_explicit_opt_in'));
	assert(testCase.destructive_reasons.includes('rest_method_mutates_state'));
	assert.equal(testCase.metadata.planned, true);
	assert.equal(testCase.metadata.gated, true);
	assert.equal(testCase.metadata.safety.requires_explicit_opt_in, true);
	assert.deepEqual(testCase.metadata.auth, { required: true, source: 'permission_callback', capability: 'edit_posts' });
}

const resourcePlan = buildWordPressFuzzPlanFromSurfaces({
	surfaces: [
		{ id: 'rest:settings', type: 'rest-route', route: '/wp/v2/settings', resource_type: 'setting', allowCrudMutations: true },
		{ id: 'custom:legacy', type: 'wp-cli-command', command: 'wp option list' },
	],
});
assert.deepEqual(resourcePlan.targets[0].cases.map((testCase) => testCase.intent), ['list-settings', 'read-setting', 'create-setting', 'update-setting', 'delete-setting']);
assert.equal(resourcePlan.targets[0].cases[2].operation.resource_type, 'setting');
assert.equal(resourcePlan.targets[0].cases[2].operation.capability_context.required[0], 'manage_options');
assert.deepEqual(resourcePlan.targets[0].cases[2].skip_reasons, []);
assert.equal(resourcePlan.targets[1].cases.length, 1);
assert.equal(resourcePlan.targets[1].cases[0].intent, 'exercise-wordpress-surface');

console.log('WordPress fuzz plan from surfaces smoke passed.');
