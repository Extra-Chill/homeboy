'use strict';

const assert = require('node:assert/strict');
const {
	REST_DB_QUERY_PROFILE_PRESET_SCHEMA,
	REST_DB_QUERY_PROFILE_ARTIFACT_PATH,
	buildWordPressRestDbQueryProfileFuzzWorkload,
	buildWordPressRestDbQueryProfileWorkload,
	normalizePresetConfig,
} = require('../lib/wordpress-rest-db-query-profile-preset');
const { wpCodeboxFuzzSuiteInput } = require('../lib/wp-codebox-fuzz-run');

const routeScopes = [
	{
		id: 'example-public-api',
		prefixes: ['/example/v1/'],
		default_params: { per_page: 1 },
		expected_statuses: [200],
		param_rules: [
			{ pattern: '#/(items|terms)(?:/|$)#', params: { per_page: 1 } },
		],
	},
];

const config = normalizePresetConfig({ routeScopes, caseLimit: 12 });
assert.equal(config.route_scopes[0].id, 'example-public-api');
assert.equal(config.case_limit, 12);

const workload = buildWordPressRestDbQueryProfileWorkload({ routeScopes, caseLimit: 12 });
assert.equal(workload.id, 'rest-db-query-profile');
assert.equal(workload.metadata.preset, REST_DB_QUERY_PROFILE_PRESET_SCHEMA);
assert.equal(workload.run[0].type, 'php');
assert.match(workload.run[0].code, /WP_CODEBOX_BENCH_SHARED_STATE/);
assert.doesNotMatch(workload.run[0].code, /HOMEBOY_BENCH_SHARED_STATE/);
assert.match(workload.run[0].code, /example-public-api/);
assert.match(workload.run[0].code, /registered-rest-route-inventory/);

const manifest = buildWordPressRestDbQueryProfileFuzzWorkload({
	slug: 'example-plugin',
	component: 'example-plugin',
	activation: 'example-plugin/example-plugin.php',
	routeScopes,
});
assert.equal(manifest.schema, 'homeboy/fuzz-workload/v1');
assert.equal(manifest.metadata.preset, REST_DB_QUERY_PROFILE_PRESET_SCHEMA);
assert.equal(manifest.workload.type, 'inline');
assert.equal(manifest.workload.definition.run[0].type, 'php');
assert.equal(manifest.cases[0].artifacts[0].path, REST_DB_QUERY_PROFILE_ARTIFACT_PATH);
assert.equal(manifest.cases[0].intent.plugin.activation, 'example-plugin/example-plugin.php');
assert.equal(manifest.target.slug, 'example-plugin');

const suiteInput = wpCodeboxFuzzSuiteInput({ id: 'rest-db-query-profile-run', homeboyFuzzWorkload: manifest });
assert.equal(suiteInput.cases[0].target.entrypoint, 'wordpress.run-workload');
assert.equal(suiteInput.cases[0].input.schema, 'wp-codebox/wordpress-workload-run/v1');
assert.equal(suiteInput.cases[0].input.steps[0].type, 'php');
assert.equal(suiteInput.cases[0].input.metadata.source, 'inline');
assert.deepEqual(suiteInput.cases[0].phases.setup, [{ command: 'wordpress.plugin-state', args: ['action=activate', 'plugin=example-plugin/example-plugin.php'] }]);
assert.equal(JSON.stringify(suiteInput).includes('wordpress.ensure-plugin-active'), false);
assert.deepEqual(suiteInput.cases[0].phases.action, [{ command: 'wordpress.run-workload' }]);

const explicitOnly = buildWordPressRestDbQueryProfileWorkload({
	restRequestCases: [
		{ id: 'example-root', path: '/example/v1/root', expected_statuses: [200] },
	],
});
assert.match(explicitOnly.run[0].code, /example-root/);

assert.throws(() => normalizePresetConfig({}), /route_scopes or rest_request_cases/);
assert.throws(() => normalizePresetConfig({ routeScopes: [{ id: 'bad' }] }), /prefixes or patterns/);

console.log('WordPress REST DB query profile preset smoke passed');
