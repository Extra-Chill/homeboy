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
		tables: { posts: { table: 'wp_posts', mutations: [{ id: 'insert-sentinel-row', operation: 'insert' }] } },
		queries: { postsLookup: { query: 'SELECT ID FROM wp_posts WHERE post_type = ?', mutation: { operation: 'update', statement: 'UPDATE wp_posts SET post_title = ? WHERE ID = ?' } } },
	},
	external_http: { requests: [{ id: 'http:api-example', url: 'https://api.example.test/v1/', method: 'GET' }] },
	blocks: [{
		id: 'block:core-paragraph',
		name: 'core/paragraph',
		block_name: 'core/paragraph',
		attributes: { content: { type: 'string' } },
		attributes_sample: { content: 'Sample paragraph' },
	}],
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
assert.equal(targetTypes.option.cases[0].execution_tier, 'read_only_executable');
assert.equal(targetTypes.option.cases[1].operation.capability_context.required[0], 'manage_options');
assert.deepEqual(targetTypes.option.cases[1].skip_reasons, ['crud_mutation_requires_explicit_allow']);
assert.equal(targetTypes.option.cases[1].execution_tier, 'plan_only');
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
assert.equal(targetTypes['database-table'].cases[1].intent, 'mutate-database-table');
assert.equal(targetTypes['database-table'].cases[1].executable, false);
assert.equal(targetTypes['database-table'].cases[1].execution_tier, 'plan_only');
assert.deepEqual(targetTypes['database-table'].cases[1].required_capabilities, ['database', 'reset', 'snapshot', 'transaction']);
assert.deepEqual(targetTypes['database-table'].cases[1].skip_reasons, ['missing-runtime-fuzz-capabilities']);
assert.deepEqual(targetTypes['database-table'].cases[1].metadata.missing_capabilities, ['database', 'reset', 'snapshot', 'transaction']);
assert.deepEqual(targetTypes['database-table'].cases[1].destructive_reasons, ['db-mutation']);
assert.equal(targetTypes['db-query'].cases[1].intent, 'mutate-database-query');
assert.equal(targetTypes['db-query'].cases[1].operation.statement, 'UPDATE wp_posts SET post_title = ? WHERE ID = ?');
assert.equal(targetTypes['external-http'].cases[0].intent, 'exercise-external-http-guardrail');
assert.equal(targetTypes['db-query'].cases[0].operation.query, 'SELECT ID FROM wp_posts WHERE post_type = ?');
assert.equal(targetTypes['external-http'].cases[0].operation.url, 'https://api.example.test/v1/');
assert(targetTypes.role.operation_id.includes('check-role-boundary'));
assert.equal(targetTypes.block.cases[0].operation.block_name, 'core/paragraph');
assert.deepEqual(targetTypes.block.cases.map((testCase) => testCase.intent), ['render-block', 'serialize-parse-block', 'insert-block-in-editor']);
assert.equal(targetTypes.block.cases[0].metadata.safety.mutation, 'read_only');
assert.equal(targetTypes.block.cases[0].execution_tier, 'read_only_executable');
assert.deepEqual(targetTypes.block.cases[1].metadata.attributes_schema, { content: { type: 'string' } });
assert.deepEqual(targetTypes.block.cases[1].metadata.attributes_sample, { content: 'Sample paragraph' });
assert.equal(targetTypes.block.cases[2].metadata.planned, true);
assert.equal(targetTypes.block.cases[2].metadata.gated, true);
assert.equal(targetTypes.block.cases[2].execution_tier, 'plan_only');
assert.deepEqual(targetTypes.block.cases[2].metadata.requires_runtime, ['browser', 'block-editor']);
assert(targetTypes.block.cases[2].skip_reasons.includes('requires_browser_editor_runtime'));
assert.equal(targetTypes['frontend-url'].cases[0].operation.path, '/');
assert.equal(targetTypes['admin-page'].cases[0].operation.path, '/wp-admin/edit.php');
assert.equal(targetTypes['rest-route'].cases[0].operation.route, '/wp/v2/posts');
assert.equal(targetTypes['rest-route'].cases[0].seed, 'seed-1');
assert(targetTypes['rest-route'].cases[0].execution_tier === 'read_only_executable');
assert(plan.metadata.execution_tiers.read_only_executable > 0);
assert(plan.metadata.execution_tiers.plan_only > 0);
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
		{ type: 'hook', hook: 'init' },
		{ type: 'cron_event', event: 'wp_version_check' },
		{ type: 'option', option: 'blogname' },
		{ type: 'role', role: 'editor' },
		{ type: 'media', name: 'attachment' },
		{ type: 'db_query', query: 'SELECT ID FROM wp_posts' },
		{ type: 'wp_cli_command', command: 'wp option list' },
	],
});
assert.deepEqual(runtimeDiscovery.surfaces.map((surface) => surface.type), ['admin_page', 'ajax_action', 'db_table', 'rest_route']);
assert.deepEqual(runtimeDiscovery.unsupported_surfaces.map((surface) => surface.type), ['cron_event', 'db_query', 'hook', 'media', 'option', 'role', 'wp_cli_command']);
assert.equal(runtimeDiscovery.unsupported_surfaces.every((surface) => surface.executable === false && surface.coverage_counted === false), true);
assert.equal(runtimeDiscovery.unsupported_surfaces.every((surface) => surface.execution_tier === 'discovered'), true);
assert.equal(runtimeDiscovery.diagnostics.every((diagnostic) => diagnostic.code === 'wordpress_surface_discovered_without_executable_runtime_collector'), true);
const runtimePlan = buildWordPressFuzzPlanFromSurfaces(runtimeDiscovery);
assert.deepEqual(runtimePlan.targets.map((target) => target.type), ['admin-page', 'ajax-action', 'database-table', 'rest-route']);
assert.equal(runtimePlan.targets.find((target) => target.type === 'ajax-action').cases[0].intent, 'exercise-ajax-action');

const wpCliRuntimeDiscovery = normalizeWordPressRuntimeSurfaceDiscovery({ wp_cli_commands: [{ command: 'wp cron event list' }] });
assert.equal(wpCliRuntimeDiscovery.unsupported_surfaces[0].id, 'wp-cli:wp cron event list');
assert.equal(wpCliRuntimeDiscovery.diagnostics[0].surface.type, 'wp_cli_command');

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
assert.equal(getCase.execution_tier, 'read_only_executable');
assert.equal(getCase.metadata.surface.args.count, 2);
assert.equal(getCase.metadata.surface.args.args[1].required, true);
assert.equal(getCase.seed, 'seed-rest');
for (const method of ['POST', 'DELETE']) {
	const testCase = restTarget.cases.find((entry) => entry.operation.method === method);
	assert(testCase.skip_reasons.includes('mutating_rest_method_requires_explicit_opt_in'));
	assert(testCase.skip_reasons.includes('missing-runtime-fuzz-capabilities'));
	assert(testCase.destructive_reasons.includes('rest_method_mutates_state'));
	assert.deepEqual(testCase.required_capabilities, ['reset', 'rest', 'restore', 'snapshot']);
	assert.equal(testCase.metadata.planned, true);
	assert.equal(testCase.metadata.gated, true);
	assert.equal(testCase.execution_tier, 'plan_only');
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
assert.deepEqual(resourcePlan.targets[0].cases[2].skip_reasons, ['requires-isolated-mutation-runtime']);
assert.equal(resourcePlan.targets[0].cases[2].executable, false);
assert.deepEqual(resourcePlan.targets[0].cases[2].required_capabilities, ['crud', 'reset', 'restore', 'snapshot']);
assert.equal(resourcePlan.targets[0].cases[2].execution_tier, 'plan_only');
assert.equal(resourcePlan.targets[1].cases.length, 1);
assert.equal(resourcePlan.targets[1].cases[0].intent, 'exercise-wordpress-surface');

const adminInteractionPlan = buildWordPressFuzzPlanFromSurfaces({
	admin: [{
		id: 'admin:bulk-posts',
		path: '/wp-admin/edit.php',
		forms: [{
			id: 'bulk-action',
			method: 'POST',
			selector: '#posts-filter',
			capability: 'edit_posts',
			nonce_action: 'bulk-posts',
		}],
		actions: [{ id: 'date-filter', method: 'GET', selector: '#filter-by-date' }],
	}],
});
const adminCases = adminInteractionPlan.targets[0].cases;
assert.equal(adminCases.length, 3);
assert.equal(adminCases[0].intent, 'request-admin-page');
assert.equal(adminCases[0].metadata.executable, true);
assert.equal(adminCases[0].execution_tier, 'read_only_executable');
assert.equal(adminCases[1].intent, 'plan-admin-page-mutation');
assert.deepEqual(adminCases[1].skip_reasons, ['missing-runtime-fuzz-capabilities', 'requires_explicit_mutation_opt_in']);
assert.deepEqual(adminCases[1].destructive_reasons, ['form_mutation']);
assert.equal(adminCases[1].metadata.executable, false);
assert.equal(adminCases[1].metadata.gated, true);
assert.equal(adminCases[1].execution_tier, 'plan_only');
assert.deepEqual(adminCases[1].required_capabilities, ['admin', 'reset', 'restore', 'snapshot']);
assert(adminCases[1].skip_reasons.includes('missing-runtime-fuzz-capabilities'));
assert.deepEqual(adminCases[1].metadata.capability_context, { required: ['edit_posts'] });
assert.deepEqual(adminCases[1].metadata.nonce_context, { required: true, action: 'bulk-posts', field: '_wpnonce' });
assert.equal(adminCases[2].intent, 'exercise-admin-page-read-only-interaction');
assert.deepEqual(adminCases[2].skip_reasons, []);
assert.deepEqual(adminCases[2].destructive_reasons, []);
assert.equal(adminCases[2].metadata.executable, true);
assert.equal(adminCases[2].execution_tier, 'read_only_executable');

const minimalBlockPlan = buildWordPressFuzzPlanFromSurfaces({
	blocks: [{ id: 'block:minimal', block_name: 'example/minimal' }],
});
assert.deepEqual(minimalBlockPlan.targets[0].cases.map((testCase) => testCase.intent), ['render-block', 'insert-block-in-editor']);

const capablePlan = buildWordPressFuzzPlanFromSurfaces({
	post_types: [{ id: 'post:post', post_type: 'post', allowCrudMutations: true }],
	database: { tables: { posts: { table: 'wp_posts', mutations: [{ id: 'insert-row', operation: 'insert' }] } } },
	admin: [{ id: 'admin:settings', forms: [{ id: 'submit', method: 'POST' }] }],
	rest: [{ id: 'rest:posts', route: '/wp/v2/posts', methods: ['POST'] }],
}, {
	runtimeCapabilities: {
		capabilities: ['crud', 'rest', 'admin', 'database', 'snapshot', 'restore', 'transaction', 'reset'],
	},
});
const capableCases = capablePlan.targets.flatMap((target) => target.cases);
for (const testCase of capableCases.filter((entry) => entry.required_capabilities && !entry.skip_reasons.length)) {
	assert.equal(testCase.executable, true);
	assert.equal(testCase.metadata.gated, false);
	assert.equal(testCase.metadata.runtime_capability_gated, false);
}
const capableCrudMutation = capableCases.find((entry) => entry.intent === 'create-post');
assert.equal(capableCrudMutation.executable, false);
assert.deepEqual(capableCrudMutation.required_capabilities, ['crud', 'reset', 'restore', 'snapshot']);
assert.deepEqual(capableCrudMutation.skip_reasons, ['requires-isolated-mutation-runtime']);
assert.equal(capableCrudMutation.execution_tier, 'plan_only');
const capableRestMutation = capableCases.find((entry) => entry.intent === 'request-rest-route');
assert.equal(capableRestMutation.executable, false);
assert.equal(capableRestMutation.metadata.runtime_capability_gated, false);
assert.deepEqual(capableRestMutation.skip_reasons, ['mutating_rest_method_requires_explicit_opt_in', 'requires-isolated-mutation-runtime']);
assert.equal(capableRestMutation.execution_tier, 'plan_only');
const capableAdminMutation = capableCases.find((entry) => entry.intent === 'plan-admin-page-mutation');
assert.equal(capableAdminMutation.executable, false);
assert.equal(capableAdminMutation.metadata.runtime_capability_gated, false);
assert.deepEqual(capableAdminMutation.skip_reasons, ['requires-isolated-mutation-runtime', 'requires_explicit_mutation_opt_in']);
assert.equal(capableAdminMutation.execution_tier, 'plan_only');

const isolatedMutationPlan = buildWordPressFuzzPlanFromSurfaces({
	post_types: [{ id: 'post:isolated', post_type: 'post' }],
	admin: [{ id: 'admin:isolated', forms: [{ id: 'submit', method: 'POST' }] }],
	rest: [{ id: 'rest:isolated', route: '/wp/v2/posts', methods: ['POST'] }],
}, {
	mutation_mode: 'isolated',
	runtimeCapabilities: {
		capabilities: ['crud', 'rest', 'admin', 'snapshot', 'restore', 'reset'],
	},
});
assert.equal(isolatedMutationPlan.metadata.mutation_mode, 'isolated');
const isolatedCases = isolatedMutationPlan.targets.flatMap((target) => target.cases);
for (const intent of ['create-post', 'request-rest-route', 'plan-admin-page-mutation']) {
	const testCase = isolatedCases.find((entry) => entry.intent === intent);
	assert.equal(testCase.executable, true, `${intent} should execute in isolated mode with runtime capabilities`);
	assert.deepEqual(testCase.skip_reasons, []);
	assert.equal(testCase.execution_tier, 'isolated_mutating_executable');
	assert.equal(testCase.metadata.runtime_capability_gated, false);
}

const readOnlyMutationPlan = buildWordPressFuzzPlanFromSurfaces({
	post_types: [{ id: 'post:read-only', post_type: 'post', allowCrudMutations: true }],
}, {
	mutation_mode: 'read_only',
	runtimeCapabilities: {
		capabilities: ['crud', 'snapshot', 'restore', 'reset'],
	},
});
const readOnlyCreate = readOnlyMutationPlan.targets[0].cases.find((entry) => entry.intent === 'create-post');
assert.equal(readOnlyCreate.executable, false);
assert.equal(readOnlyCreate.metadata.mutation_policy_gated, true);
assert.equal(readOnlyCreate.metadata.mutation_policy.mode, 'read_only');
assert.deepEqual(readOnlyCreate.skip_reasons, ['mutation-policy-read-only']);
assert.equal(readOnlyCreate.execution_tier, 'plan_only');

console.log('WordPress fuzz plan from surfaces smoke passed.');
