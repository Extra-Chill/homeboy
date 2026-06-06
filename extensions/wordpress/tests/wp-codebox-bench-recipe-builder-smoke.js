const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const path = require('node:path');

const script = path.join(__dirname, '..', 'scripts', 'bench', 'build-wp-codebox-bench-recipe.mjs');
const fixtureCoreModule = path.join(__dirname, 'fixtures', 'wp-codebox-core-recipe-builder.mjs');
const missingBenchBuilderModule = path.join(__dirname, 'fixtures', 'wp-codebox-core-missing-bench-builder.mjs');

const input = {
	options: {
		componentId: 'fixture-component',
		pluginSlug: 'fixture-plugin',
		iterations: 5,
		warmupIterations: 0,
		dependencySlugs: ['dependency-one', 'dependency-two'],
		env: { FIXTURE: '1' },
		wpConfigDefines: { WP_DEBUG: true },
		bootstrapFiles: ['/wordpress/wp-content/plugins/fixture-plugin/bootstrap.php'],
		workloads: [{ id: 'fixture-workload', steps: [{ type: 'php', code: 'return [];' }] }],
		lifecycle: { before: ['fixture'] },
		resetPolicy: { mode: 'snapshot' },
		pluginRuntime: {
			url: 'https://example.com/runtime.zip',
			sha256: 'abc123',
			setup: [{ command: 'wordpress.wp-cli', args: ['command=option update fixture_bootstrap yes'] }],
		},
		mounts: [{ source: '/tmp/fixture-plugin', target: '/wordpress/wp-content/plugins/fixture-plugin' }],
		extraPlugins: [{ source: '/tmp/extra-plugin.zip', slug: 'extra-plugin', activate: true }],
	},
};

const result = spawnSync(process.execPath, [script], {
	cwd: path.join(__dirname, '..'),
	input: JSON.stringify(input),
	encoding: 'utf8',
	env: { ...process.env, HOMEBOY_WP_CODEBOX_CORE_MODULE: fixtureCoreModule },
});

assert.equal(result.status, 0, result.stderr);
const recipe = JSON.parse(result.stdout);

assert.equal(recipe.schema, 'wp-codebox/workspace-recipe/v1');
assert.equal(recipe.inputs.mounts[0].mode, 'readonly');
assert.deepEqual(recipe.inputs.extraPlugins[0], { source: '/tmp/extra-plugin.zip', slug: 'extra-plugin', activate: true });
assert.deepEqual(recipe.inputs.pluginRuntime, input.options.pluginRuntime);
assert.deepEqual(recipe.workflow.steps, [{
	command: 'fixture.wordpress.bench',
	args: [
		'plugin-slug=fixture-plugin',
		'lifecycle-json={"before":["fixture"]}',
		'reset-policy-json={"mode":"snapshot"}',
	],
}]);

const diagnosticResult = spawnSync(process.execPath, [script], {
	cwd: path.join(__dirname, '..'),
	input: JSON.stringify(input),
	encoding: 'utf8',
	env: { ...process.env, HOMEBOY_WP_CODEBOX_CORE_MODULE: '/missing/wp-codebox-core.mjs' },
});

assert.notEqual(diagnosticResult.status, 0);
assert.match(diagnosticResult.stderr, /WP Codebox recipe builder export buildWordPressBenchRecipe is unavailable/);
assert.match(diagnosticResult.stderr, /no longer falls back to bundled WP Codebox recipe builders/);
assert.match(diagnosticResult.stderr, /\/missing\/wp-codebox-core\.mjs/);

const staleModuleResult = spawnSync(process.execPath, [script], {
	cwd: path.join(__dirname, '..'),
	input: JSON.stringify(input),
	encoding: 'utf8',
	env: { ...process.env, HOMEBOY_WP_CODEBOX_CORE_MODULE: missingBenchBuilderModule },
});

assert.notEqual(staleModuleResult.status, 0);
assert.match(staleModuleResult.stderr, /missing buildWordPressBenchRecipe export/);

const invalidMountResult = spawnSync(process.execPath, [script], {
	cwd: path.join(__dirname, '..'),
	input: JSON.stringify({ options: { pluginSlug: 'fixture-plugin', mounts: [{ source: '/tmp/plugin', target: 'relative/path' }] } }),
	encoding: 'utf8',
	env: { ...process.env, HOMEBOY_WP_CODEBOX_CORE_MODULE: fixtureCoreModule },
});

assert.notEqual(invalidMountResult.status, 0);
assert.match(invalidMountResult.stderr, /Recipe mount 0 requires an absolute target/);

console.log('wp-codebox bench recipe builder smoke passed');
