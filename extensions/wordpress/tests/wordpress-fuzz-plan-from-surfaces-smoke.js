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
assert.equal(targetTypes['database-table'].cases[1].metadata.mutation_lifecycle.schema, 'homeboy/wordpress-fuzz-mutation-lifecycle/v1');
assert.deepEqual(targetTypes['database-table'].cases[1].metadata.mutation_lifecycle.required_capabilities, ['database', 'reset', 'snapshot', 'transaction']);
assert(targetTypes['database-table'].cases[1].metadata.mutation_lifecycle.required_evidence.some((entry) => entry.kind === 'transaction'));
assert.equal(targetTypes['db-query'].cases[1].intent, 'mutate-database-query');
assert.equal(targetTypes['db-query'].cases[1].operation.statement, 'UPDATE wp_posts SET post_title = ? WHERE ID = ?');
assert.equal(targetTypes['external-http'].cases[0].intent, 'exercise-external-http-guardrail');
assert.equal(targetTypes['db-query'].cases[0].operation.query, 'SELECT ID FROM wp_posts WHERE post_type = ?');
assert.equal(targetTypes['external-http'].cases[0].operation.url, 'https://api.example.test/v1/');
assert(targetTypes.role.operation_id.includes('check-role-boundary'));
assert.equal(targetTypes['rest-route'].cases[0].runtime_operation.command, 'wordpress.request-rest-route');
assert.equal(targetTypes['admin-page'].cases[0].runtime_operation.command, 'wordpress.load-admin-page');
assert.equal(targetTypes['frontend-url'].cases[0].runtime_operation.command, 'wordpress.load-frontend-page');
assert.equal(targetTypes.block.cases[0].runtime_operation.command, 'wordpress.exercise-block');
assert.equal(targetTypes['db-query'].cases[0].runtime_operation.command, 'wordpress.profile-database');
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
assert.equal(targetTypes['frontend-url'].cases.length, 1, 'frontend random-walk generation stays disabled outside aggressive isolated mode');
assert.equal(targetTypes['admin-page'].cases[0].operation.path, '/wp-admin/edit.php');
assert.equal(targetTypes['admin-page'].cases.length, 1, 'admin action discovery and random-walk generation stay disabled outside aggressive isolated mode');
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
	assert.deepEqual(testCase.required_capabilities, ['checkpoint', 'rest', 'rest-rollback']);
	assert.deepEqual(testCase.metadata.required_any_capabilities, [['reset', 'restore']]);
	assert.equal(testCase.metadata.mutation_lifecycle.schema, 'homeboy/wordpress-fuzz-mutation-lifecycle/v1');
	assert.deepEqual(testCase.metadata.mutation_lifecycle.required_capabilities, ['checkpoint', 'rest', 'rest-rollback']);
	assert.equal(testCase.metadata.mutation_lifecycle.delete_boundary_required, method === 'DELETE');
	assert.equal(testCase.metadata.rollback_contract.schema, 'homeboy/wordpress-rest-mutation-rollback-contract/v1');
	assert.equal(testCase.metadata.rollback_contract.delete_boundary_artifacts, method === 'DELETE');
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
assert.deepEqual(resourcePlan.targets[0].cases[2].required_capabilities, ['checkpoint', 'crud', 'rest', 'rest-rollback']);
assert.deepEqual(resourcePlan.targets[0].cases[2].metadata.mutation_lifecycle.required_capabilities, ['checkpoint', 'crud', 'rest', 'rest-rollback']);
assert.equal(resourcePlan.targets[0].cases[2].metadata.rollback_contract.schema, 'homeboy/wordpress-rest-mutation-rollback-contract/v1');
assert.equal(resourcePlan.targets[0].cases[2].execution_tier, 'plan_only');
assert.equal(resourcePlan.targets[1].cases.length, 1);
assert.equal(resourcePlan.targets[1].cases[0].intent, 'exercise-wordpress-surface');

const randomWalkExecutablePlan = buildWordPressFuzzPlanFromSurfaces({
	frontend: [{ id: 'front:walk', path: '/walk/' }],
	admin: [{ id: 'admin:walk', path: '/wp-admin/tools.php' }],
	blocks: [{ id: 'block:walk', block_name: 'core/paragraph' }],
}, {
	seed: 'walk-seed',
	mutation_mode: 'aggressive-isolated',
	runtimeCapabilities: { capabilities: ['browser', 'block-editor', 'snapshot', 'restore', 'reset'] },
	randomWalkMaxSteps: 6,
	randomWalkActionFamilies: ['click', 'capture'],
});
const randomWalkCases = randomWalkExecutablePlan.targets.flatMap((target) => target.cases).filter((testCase) => testCase.input?.type === 'random_walk');
assert.deepEqual([...randomWalkCases.map((testCase) => testCase.intent)].sort(), ['admin-random-walk', 'browser-random-walk', 'editor-random-walk'].sort());
assert.equal(randomWalkCases.find((testCase) => testCase.intent === 'browser-random-walk').execution_tier, 'isolated_mutating_executable');
assert(randomWalkCases.every((testCase) => JSON.stringify(testCase.input.action_families) === JSON.stringify(['click', 'capture'])));
assert(randomWalkCases.every((testCase) => testCase.metadata.random_walk.maxSteps === 6));
assert.equal(randomWalkCases.find((testCase) => testCase.intent === 'admin-random-walk').metadata.random_walk.seed, 'walk-seed:admin:walk:admin');
assert.equal(randomWalkExecutablePlan.targets.find((target) => target.type === 'admin-page').cases[1].intent, 'discover-admin-page-actions');
assert.equal(randomWalkExecutablePlan.targets.find((target) => target.type === 'admin-page').cases[1].metadata.action_discovery.executes_actions, false);

const fixtureBoundCrudPlan = buildWordPressFuzzPlanFromSurfaces({
	surfaces: [{
		id: 'rest:items-id',
		type: 'rest-route',
		resource_type: 'item',
		route: '/example/v1/items/(?P<id>[\\d]+)',
		allowCrudMutations: true,
		fixture_bindings: {
			fixture_id: 'fixture:item:primary',
			artifact_ref: { id: 'artifact:item-manifest', path: 'fixtures/items.json' },
			route_params: {
				id: { value: 42, artifact_ref: { id: 'artifact:item-id', path: 'fixtures/items.json', pointer: '/primary/id' } },
			},
			request_bodies: {
				create: { title: 'Created fixture item' },
				update: { title: 'Updated fixture item' },
				delete: { force: true },
			},
		},
	}],
}, {
	mutation_mode: 'isolated',
	runtimeCapabilities: {
		capabilities: ['crud', 'snapshot', 'restore', 'reset'],
	},
});
const fixtureBoundCases = fixtureBoundCrudPlan.targets[0].cases;
assert.deepEqual(fixtureBoundCases.map((testCase) => testCase.intent), ['list-items', 'read-item', 'create-item', 'update-item', 'delete-item']);
for (const testCase of fixtureBoundCases) {
	assert.equal(testCase.operation.transport.route, '/example/v1/items/42');
	assert.equal(testCase.operation.transport.metadata.route_template, '/example/v1/items/(?P<id>[\\d]+)');
	assert.deepEqual(testCase.operation.input.route_params, { id: 42 });
	assert.equal(testCase.metadata.fixture_binding.fixture_id, 'fixture:item:primary');
	assert.deepEqual(testCase.metadata.fixture_binding.route_params.id.artifact_ref, { id: 'artifact:item-id', path: 'fixtures/items.json', pointer: '/primary/id' });
}
assert.deepEqual(fixtureBoundCases.find((testCase) => testCase.intent === 'create-item').operation.input.request_body, { title: 'Created fixture item' });
assert.deepEqual(fixtureBoundCases.find((testCase) => testCase.intent === 'update-item').operation.input.request_body, { title: 'Updated fixture item' });
assert.deepEqual(fixtureBoundCases.find((testCase) => testCase.intent === 'delete-item').operation.input.request_body, { force: true });
assert.equal(fixtureBoundCases.find((testCase) => testCase.intent === 'read-item').operation.input.request_body, undefined);

const unsupportedRuntimeOperationPlan = buildWordPressFuzzPlanFromSurfaces({
	blocks: [{ id: 'block:unsupported-runtime', block_name: 'example/runtime' }],
}, { runtimeCapabilities: { capabilities: [] } });
const unsupportedBlockCase = unsupportedRuntimeOperationPlan.targets[0].cases[0];
assert.equal(unsupportedBlockCase.executable, false);
assert.equal(unsupportedBlockCase.execution_tier, 'plan_only');
assert.equal(unsupportedBlockCase.runtime_operation.status, 'skipped');
assert.deepEqual(unsupportedBlockCase.runtime_operation.missing_capabilities, ['block']);
assert(unsupportedBlockCase.skip_reasons.includes('missing-runtime-workload-capability'));

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
assert.equal(adminCases[1].metadata.mutation_lifecycle.kind, 'admin');
assert(adminCases[1].metadata.mutation_lifecycle.required_evidence.some((entry) => entry.kind === 'restore'));
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
		capabilities: ['crud', 'rest', 'admin', 'database', 'snapshot', 'restore', 'transaction', 'reset', 'checkpoint', 'rest-rollback'],
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
assert.equal(capableCrudMutation.metadata.mutation_lifecycle.kind, 'crud');
assert.deepEqual(capableCrudMutation.skip_reasons, ['requires-isolated-mutation-runtime']);
assert.equal(capableCrudMutation.execution_tier, 'plan_only');
const capableRestMutation = capableCases.find((entry) => entry.intent === 'request-rest-route');
assert.equal(capableRestMutation.executable, false);
assert.equal(capableRestMutation.metadata.runtime_capability_gated, false);
assert.deepEqual(capableRestMutation.skip_reasons, ['mutating_rest_method_requires_explicit_opt_in', 'requires-isolated-mutation-runtime']);
assert.equal(capableRestMutation.execution_tier, 'plan_only');
const capableDbMutation = capableCases.find((entry) => entry.intent === 'mutate-database-table');
assert.equal(capableDbMutation.executable, false);
assert.equal(capableDbMutation.metadata.runtime_capability_gated, false);
assert.deepEqual(capableDbMutation.skip_reasons, ['requires-isolated-mutation-runtime']);
assert.equal(capableDbMutation.execution_tier, 'plan_only');
assert.equal(capableDbMutation.runtime_operation.status, 'ready');
const capableAdminMutation = capableCases.find((entry) => entry.intent === 'plan-admin-page-mutation');
assert.equal(capableAdminMutation.executable, false);
assert.equal(capableAdminMutation.metadata.runtime_capability_gated, false);
assert.deepEqual(capableAdminMutation.skip_reasons, ['invalid-runtime-workload-operation', 'requires-isolated-mutation-runtime', 'requires_explicit_mutation_opt_in']);
assert.equal(capableAdminMutation.runtime_operation.status, 'blocked');
assert.equal(capableAdminMutation.runtime_operation.blockers[0].code, 'missing-runtime-workload-operation-field');
assert.equal(capableAdminMutation.execution_tier, 'plan_only');

const isolatedMutationPlan = buildWordPressFuzzPlanFromSurfaces({
	post_types: [{ id: 'post:isolated', post_type: 'post' }],
	admin: [{ id: 'admin:isolated', forms: [{ id: 'submit', method: 'POST' }] }],
	rest: [{ id: 'rest:isolated', route: '/wp/v2/posts', methods: ['POST'] }],
}, {
	mutation_mode: 'isolated',
	runtimeCapabilities: {
		capabilities: ['crud', 'rest', 'admin', 'snapshot', 'restore', 'reset', 'checkpoint', 'rest-rollback'],
	},
});
assert.equal(isolatedMutationPlan.metadata.mutation_mode, 'isolated');
const isolatedCases = isolatedMutationPlan.targets.flatMap((target) => target.cases);
for (const intent of ['create-post', 'request-rest-route']) {
	const testCase = isolatedCases.find((entry) => entry.intent === intent);
	assert.equal(testCase.executable, true, `${intent} should execute in isolated mode with runtime capabilities`);
	assert.deepEqual(testCase.skip_reasons, []);
	assert.equal(testCase.execution_tier, 'isolated_mutating_executable');
	assert.equal(testCase.metadata.runtime_capability_gated, false);
}
const isolatedAdminMutation = isolatedCases.find((entry) => entry.intent === 'plan-admin-page-mutation');
assert.equal(isolatedAdminMutation.executable, false);
assert.deepEqual(isolatedAdminMutation.skip_reasons, ['invalid-runtime-workload-operation']);
assert.equal(isolatedAdminMutation.runtime_operation.status, 'blocked');
assert.equal(isolatedAdminMutation.execution_tier, 'plan_only');
const isolatedRestMutation = isolatedCases.find((entry) => entry.intent === 'request-rest-route');
const isolatedCrudDelete = isolatedCases.find((entry) => entry.intent === 'delete-post');
assert.equal(isolatedRestMutation.metadata.rollback_contract.schema, 'homeboy/wordpress-rest-mutation-rollback-contract/v1');
assert.deepEqual(isolatedRestMutation.metadata.required_any_capabilities, [['reset', 'restore']]);
assert(isolatedCrudDelete.metadata.mutation_lifecycle.required_evidence.some((entry) => entry.kind === 'delete-boundary'));

const optInRestMutationPlan = buildWordPressFuzzPlanFromSurfaces({
	rest: [{ id: 'rest:generic-items', route: '/example/v1/items/(?P<id>[\\d]+)', methods: ['POST', 'PATCH', 'DELETE'] }],
}, {
	mutation_mode: 'isolated',
	runtimeCapabilities: { capabilities: ['rest', 'checkpoint', 'rest-rollback', 'restore'] },
	runtimeReadiness: {
		schema: 'wp-codebox/fuzz-runner-readiness/v1',
		status: 'ready',
		operationKinds: ['mutation'],
		mutationIsolation: true,
		deleteBoundary: true,
	},
	rest_mutation_opt_ins: {
		id: 'generic-rest-mutation-opt-ins',
		entries: [
			{ id: 'create-item-opt-in', route: '/example/v1/items/(?P<id>[\\d]+)', method: 'POST', fixture_ref: 'fixture:items' },
			{ id: 'patch-item-opt-in', route: '/example/v1/items/(?P<id>[\\d]+)', method: 'PATCH', contract_ref: 'contract:patch' },
			{ id: 'delete-item-opt-in', route: '/example/v1/items/(?P<id>[\\d]+)', method: 'DELETE', contract_ref: 'contract:delete' },
		],
	},
	fixture_bindings: {
		'rest:generic-items': {
			route_params: { id: 42 },
			request_bodies: {
				post: { title: 'Created fixture item' },
				patch: { title: 'Patched fixture item' },
				delete: { force: true },
			},
		},
	},
});
const optInRestCases = optInRestMutationPlan.targets[0].cases;
assert.deepEqual(optInRestCases.map((testCase) => testCase.operation.method), ['DELETE', 'PATCH', 'POST']);
for (const method of ['POST', 'PATCH', 'DELETE']) {
	const testCase = optInRestCases.find((entry) => entry.operation.method === method);
	assert.equal(testCase.executable, true, `${method} should execute with isolated readiness and opt-in`);
	assert.equal(testCase.runtime_operation.status, 'ready');
	assert.equal(testCase.operation.route, '/example/v1/items/42');
	assert.equal(testCase.metadata.rest_mutation_opt_in.method, method);
	assert.equal(testCase.metadata.fixture_binding.route_params.id.value, 42);
}
assert.deepEqual(optInRestCases.find((entry) => entry.operation.method === 'PATCH').operation.request_body, { title: 'Patched fixture item' });
assert.deepEqual(optInRestCases.find((entry) => entry.operation.method === 'DELETE').operation.request_body, { force: true });

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

const aggressiveDefaultPlan = buildWordPressFuzzPlanFromSurfaces({
	post_types: [{ id: 'post:aggressive-default', post_type: 'post', allowCrudMutations: true }],
	rest: [{ id: 'rest:aggressive-default', route: '/wp/v2/posts', methods: ['POST'] }],
}, {
	runtimeCapabilities: {
		capabilities: ['crud', 'rest', 'snapshot', 'restore', 'reset', 'checkpoint', 'rest-rollback'],
	},
});
const aggressiveDefaultCases = aggressiveDefaultPlan.targets.flatMap((target) => target.cases);
const aggressiveDefaultCreate = aggressiveDefaultCases.find((entry) => entry.intent === 'create-post');
const aggressiveDefaultRest = aggressiveDefaultCases.find((entry) => entry.intent === 'request-rest-route');
assert.equal(aggressiveDefaultCreate.executable, false);
assert.equal(aggressiveDefaultCreate.execution_tier, 'plan_only');
assert.deepEqual(aggressiveDefaultCreate.skip_reasons, ['requires-isolated-mutation-runtime']);
assert.equal(aggressiveDefaultCreate.metadata.reset.schema, 'homeboy/wordpress-fuzz-reset/v1');
assert.equal(aggressiveDefaultCreate.metadata.isolation.schema, 'homeboy/wordpress-fuzz-isolation/v1');
assert.equal(aggressiveDefaultRest.executable, false);
assert.equal(aggressiveDefaultRest.execution_tier, 'plan_only');
assert(aggressiveDefaultRest.skip_reasons.includes('mutating_rest_method_requires_explicit_opt_in'));
assert.equal(aggressiveDefaultRest.metadata.reset.boundary, 'after_each_case');
assert.deepEqual(aggressiveDefaultRest.metadata.reset.required_any_capabilities, [['restore', 'reset']]);

const aggressiveIsolatedPlan = buildWordPressFuzzPlanFromSurfaces({
	post_types: [{ id: 'post:aggressive-isolated', post_type: 'post', allowCrudMutations: true }],
	database: { tables: { posts: { table: 'wp_posts', mutations: [{ id: 'insert-row', operation: 'insert' }] } } },
	admin: [{
		id: 'admin:aggressive-isolated',
		path: '/wp-admin/edit.php',
		forms: [{
			id: 'bulk-action',
			method: 'POST',
			selector: '#posts-filter',
			fields: { action: 'edit' },
			capability: 'edit_posts',
			nonce_action: 'bulk-posts',
		}],
	}],
	rest: [{ id: 'rest:aggressive-isolated', route: '/wp/v2/posts', methods: ['POST'] }],
}, {
	mutation_mode: 'aggressive-isolated',
	runtimeCapabilities: {
		capabilities: ['crud', 'rest', 'admin', 'database', 'snapshot', 'restore', 'reset', 'transaction', 'checkpoint', 'rest-rollback'],
	},
});
assert.equal(aggressiveIsolatedPlan.metadata.mutation_mode, 'aggressive-isolated');
const aggressiveIsolatedCases = aggressiveIsolatedPlan.targets.flatMap((target) => target.cases);
for (const intent of ['create-post', 'request-rest-route', 'plan-admin-page-mutation', 'mutate-database-table']) {
	const testCase = aggressiveIsolatedCases.find((entry) => entry.intent === intent);
	assert.equal(testCase.executable, true, `${intent} should execute in aggressive isolated mode`);
	assert.deepEqual(testCase.skip_reasons, []);
	assert.equal(testCase.execution_tier, 'isolated_mutating_executable');
	assert.equal(testCase.metadata.planned, false);
	assert.equal(testCase.metadata.gated, false);
	assert.equal(testCase.metadata.isolation.mode, 'aggressive-isolated');
	assert.equal(testCase.metadata.isolation.boundary, 'per_case');
	assert.equal(testCase.metadata.reset.mode, 'aggressive-isolated');
	assert.equal(testCase.metadata.reset.boundary, 'after_each_case');
	assert.equal(testCase.runtime_operation.status, 'ready');
}
const aggressiveAdminMutation = aggressiveIsolatedCases.find((entry) => entry.intent === 'plan-admin-page-mutation');
assert.equal(aggressiveAdminMutation.runtime_operation.input.path, '/wp-admin/edit.php');
assert.equal(aggressiveAdminMutation.runtime_operation.input.interaction_kind, 'form');
assert.equal(aggressiveAdminMutation.runtime_operation.input.selector, '#posts-filter');
assert.deepEqual(aggressiveAdminMutation.runtime_operation.input.capability_context, { required: ['edit_posts'] });
assert.deepEqual(aggressiveAdminMutation.runtime_operation.input.nonce_context, { required: true, action: 'bulk-posts', field: '_wpnonce' });
const aggressiveDbMutation = aggressiveIsolatedCases.find((entry) => entry.intent === 'mutate-database-table');
assert.deepEqual(aggressiveDbMutation.required_capabilities, ['database', 'reset', 'snapshot', 'transaction']);
assert.equal(aggressiveDbMutation.metadata.mutation_lifecycle.kind, 'database');
assert.deepEqual(aggressiveDbMutation.metadata.reset.required_capabilities, ['database', 'reset', 'snapshot', 'transaction']);
assert.deepEqual(aggressiveDbMutation.runtime_operation.required_capabilities, ['database', 'reset', 'snapshot', 'transaction']);

const destructiveIsolatedAliasPlan = buildWordPressFuzzPlanFromSurfaces({
	post_types: [{ id: 'post:destructive-isolated-alias', post_type: 'post', allowCrudMutations: true }],
}, {
	mutation_mode: 'destructive_isolated',
	runtimeCapabilities: { capabilities: ['crud', 'snapshot', 'restore', 'reset'] },
});
assert.equal(destructiveIsolatedAliasPlan.metadata.mutation_mode, 'aggressive-isolated');
assert.equal(destructiveIsolatedAliasPlan.targets[0].cases.find((entry) => entry.intent === 'create-post').executable, true);

const argDrivenConservativePlan = buildWordPressFuzzPlanFromSurfaces({
	rest: [{
		id: 'rest:arg-conservative',
		route: '/example/v1/items',
		methods: ['POST'],
		args: {
			title: { type: 'string', required: true },
			tags: { type: 'array' },
		},
	}],
});
assert.equal(argDrivenConservativePlan.targets[0].cases.length, 1, 'arg-driven expansion stays disabled outside aggressive isolated mode');

const argDrivenAggressivePlan = buildWordPressFuzzPlanFromSurfaces({
	rest: [{
		id: 'rest:arg-aggressive',
		route: '/example/v1/items',
		methods: ['POST'],
		args: {
			title: { type: 'string', required: true },
			tags: { type: 'array' },
			published: { type: 'boolean' },
		},
	}],
}, {
	seed: 'seed-args',
	mutation_mode: 'aggressive-isolated',
	runtimeCapabilities: { capabilities: ['rest', 'checkpoint', 'rest-rollback', 'restore', 'reset'] },
});
const argDrivenCases = argDrivenAggressivePlan.targets[0].cases;
assert.deepEqual(argDrivenCases.map((testCase) => testCase.metadata.arg_generation.variant), ['valid-minimal', 'boundary-large', 'invalid-type']);
assert.deepEqual(argDrivenCases[0].operation.request_body, { title: 'sample' });
assert.equal(argDrivenCases[1].operation.request_body.title.length, 4096);
assert.equal(argDrivenCases[1].operation.request_body.tags.length, 16);
assert.deepEqual(argDrivenCases[2].operation.request_body.title, { invalid: 'object-for-string' });
for (const testCase of argDrivenCases) {
	assert.equal(testCase.executable, true);
	assert.equal(testCase.execution_tier, 'isolated_mutating_executable');
	assert.equal(testCase.metadata.replay.seed, 'seed-args');
	assert.equal(testCase.metadata.replay.variant, testCase.metadata.arg_generation.variant);
	assert.equal(testCase.metadata.deterministic_seed, 'seed-args');
	assert.equal(testCase.metadata.isolation.mode, 'aggressive-isolated');
	assert.equal(testCase.metadata.reset.boundary, 'after_each_case');
	assert.equal(testCase.runtime_operation.status, 'ready');
	assert.deepEqual(testCase.runtime_operation.input.request_body, testCase.operation.request_body);
}

const argDrivenReadPlan = buildWordPressFuzzPlanFromSurfaces({
	rest: [{ id: 'rest:arg-read', route: '/example/v1/items', methods: ['GET'], args: [{ name: 'search', type: 'string', required: true }] }],
}, { mutation_mode: 'aggressive-isolated', runtimeCapabilities: { capabilities: ['rest'] } });
assert.deepEqual(argDrivenReadPlan.targets[0].cases[0].operation.query_params, { search: 'sample' });
assert.deepEqual(argDrivenReadPlan.targets[0].cases[0].runtime_operation.input.query_params, { search: 'sample' });

const schemaDrivenDbPlan = buildWordPressFuzzPlanFromSurfaces({
	database: {
		tables: {
			demo: {
				id: 'db:demo',
				table: 'wp_demo',
				columns: [
					{ name: 'id', type: 'bigint', key: 'PRI', extra: 'auto_increment' },
					{ name: 'name', type: 'varchar(255)', key: '', extra: '' },
				],
				indexes: [{ name: 'PRIMARY', column: 'id', unique: true, sequence: 1 }],
			},
		},
	},
}, {
	seed: 'seed-db-schema',
	mutation_mode: 'aggressive-isolated',
	runtimeCapabilities: { capabilities: ['database', 'snapshot', 'restore', 'reset', 'transaction'] },
});
const schemaDbCases = schemaDrivenDbPlan.targets[0].cases;
assert.deepEqual(schemaDbCases.map((testCase) => testCase.intent), ['inspect-database-table', 'mutate-database-table', 'mutate-database-table', 'mutate-database-table']);
assert.deepEqual(schemaDbCases.slice(1).map((testCase) => testCase.operation.mutation), ['insert', 'update', 'delete']);
for (const testCase of schemaDbCases.slice(1)) {
	assert.equal(testCase.executable, true);
	assert.equal(testCase.execution_tier, 'isolated_mutating_executable');
	assert.equal(testCase.metadata.seed.source, 'schema-driven-db-generation');
	assert.equal(testCase.metadata.replay.seed, undefined);
	assert.equal(testCase.metadata.replay.table, 'wp_demo');
	assert.equal(testCase.runtime_operation.status, 'ready');
	assert.deepEqual(testCase.runtime_operation.input.values, testCase.operation.values);
}
assert.deepEqual(schemaDbCases.find((testCase) => testCase.operation.mutation === 'update').runtime_operation.input.where, { id: 1 });
assert.deepEqual(schemaDbCases.find((testCase) => testCase.operation.mutation === 'delete').runtime_operation.input.where, { id: 1 });

console.log('WordPress fuzz plan from surfaces smoke passed.');
