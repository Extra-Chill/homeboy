'use strict';

const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const path = require('node:path');

const {
	WP_CODEBOX_FUZZ_RUN_SCHEMA,
	WP_CODEBOX_FUZZ_SUITE_SCHEMA,
	WP_CODEBOX_WORKSPACE_RECIPE_SCHEMA,
	buildWpCodeboxFuzzPlanRecipe,
	buildWpCodeboxFuzzPlanRecipeLegacyRunAlias,
	legacyWpCodeboxFuzzRunSchemaAlias,
} = require('../lib/wp-codebox-fuzz-plan');

const plan = {
	plan_id: 'generic-fuzz-plan',
	mounts: [{ source: '/tmp/fixture-plugin', target: '/wordpress/wp-content/plugins/fixture-plugin' }],
	workflow: { steps: [{ command: 'inspect-mounted-inputs' }] },
	cases: [{
		case_id: 'case-001',
		input: { route: '/wp/v2/posts', method: 'GET' },
		input_hash: { algorithm: 'sha256', value: 'abc123' },
		phases: {
			setup: [{ command: 'wordpress.run-php', args: ["code=update_option('fuzz_case','case-001');"] }],
			action: [{ command: 'wordpress.wp-cli', args: ['command=option get fuzz_case'] }],
			assert: [{ command: 'wordpress.run-php', args: ["code=if (get_option('fuzz_case') !== 'case-001') { exit(1); }"] }],
			teardown: [{ command: 'wordpress.run-php', args: ["code=delete_option('fuzz_case');"] }],
		},
		artifacts: [{ name: 'case-log', path: '/tmp/wp-codebox/fuzz/case-001.json', required: false }],
		replay: { seed: 'seed-001', inputRef: 'fixtures/cases/case-001.json' },
	}],
	metadata: { source: 'smoke' },
};

const recipe = buildWpCodeboxFuzzPlanRecipe(plan);

assert.equal(recipe.schema, WP_CODEBOX_WORKSPACE_RECIPE_SCHEMA);
assert.equal(recipe.fuzzRun, undefined);
assert.equal(recipe.fuzzSuite.schema, WP_CODEBOX_FUZZ_SUITE_SCHEMA);
assert.equal(WP_CODEBOX_FUZZ_RUN_SCHEMA, WP_CODEBOX_FUZZ_SUITE_SCHEMA);
assert.equal(legacyWpCodeboxFuzzRunSchemaAlias(), WP_CODEBOX_FUZZ_SUITE_SCHEMA);
assert.equal(recipe.fuzzSuite.cases[0].case_id, 'case-001');
assert.deepEqual(recipe.fuzzSuite.cases[0].phases.action, [{
	command: 'wordpress.wp-cli',
	args: ['command=option get fuzz_case'],
}]);
assert.equal(recipe.inputs.mounts[0].mode, 'readonly');
assert.equal(recipe.fuzzSuite.metadata.planner, 'homeboy/wordpress-fuzz-plan-recipe-builder/v1');
assert(!JSON.stringify(recipe).includes('woocommerce'), 'fuzz plan builder must stay product-agnostic');

const legacyRecipe = buildWpCodeboxFuzzPlanRecipeLegacyRunAlias(plan);
assert.equal(legacyRecipe.fuzzSuite.schema, WP_CODEBOX_FUZZ_SUITE_SCHEMA);
assert.equal(legacyRecipe.fuzzRun, legacyRecipe.fuzzSuite);

const script = path.join(__dirname, '..', 'scripts', 'fuzz', 'build-wp-codebox-fuzz-plan-recipe.mjs');
const result = spawnSync(process.execPath, [script], {
	cwd: path.join(__dirname, '..'),
	input: JSON.stringify({ plan }),
	encoding: 'utf8',
});

assert.equal(result.status, 0, result.stderr);
const cliRecipe = JSON.parse(result.stdout);
assert.equal(cliRecipe.fuzzRun, undefined);
assert.equal(cliRecipe.fuzzSuite.schema, WP_CODEBOX_FUZZ_SUITE_SCHEMA);
assert.equal(cliRecipe.fuzzSuite.cases[0].case_id, 'case-001');

assert.throws(
	() => buildWpCodeboxFuzzPlanRecipe({ cases: [{ case_id: 'missing-action', phases: { setup: [{ command: 'wordpress.wp-cli' }] } }] }),
	/requires at least one action phase step/
);
assert.throws(
	() => buildWpCodeboxFuzzPlanRecipe({ cases: [{ phases: { action: [{ command: 'wordpress.wp-cli' }] } }] }),
	/requires case_id/
);

console.log('wp-codebox fuzz plan recipe builder smoke passed');
