const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const path = require('node:path');

const script = path.join(__dirname, '..', 'scripts', 'bench', 'build-wp-codebox-bench-recipe.mjs');

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
	env: { ...process.env, HOMEBOY_WP_CODEBOX_CORE_MODULE: '' },
});

assert.equal(result.status, 0, result.stderr);
const recipe = JSON.parse(result.stdout);

assert.equal(recipe.schema, 'wp-codebox/workspace-recipe/v1');
assert.equal(recipe.inputs.mounts[0].mode, 'readonly');
assert.deepEqual(recipe.inputs.extraPlugins[0], { source: '/tmp/extra-plugin.zip', slug: 'extra-plugin', activate: true });
assert.deepEqual(recipe.inputs.pluginRuntime, input.options.pluginRuntime);
assert.equal(recipe.runtime.blueprint.steps[0].step, 'defineWpConfigConsts');
assert.deepEqual(recipe.runtime.blueprint.steps[0].consts, { WP_DEBUG: true });
assert.deepEqual(recipe.workflow.steps, [{
	command: 'wordpress.bench',
	args: [
		'component-id=fixture-component',
		'plugin-slug=fixture-plugin',
		'iterations=5',
		'warmup=0',
		'dependency-slugs=dependency-one,dependency-two',
		'env-json={"FIXTURE":"1"}',
		'bootstrap-files-json=["/wordpress/wp-content/plugins/fixture-plugin/bootstrap.php"]',
		'workloads-json=[{"id":"fixture-workload","steps":[{"type":"php","code":"return [];"}]}]',
	],
}]);

const diagnosticResult = spawnSync(process.execPath, [script], {
	cwd: path.join(__dirname, '..'),
	input: JSON.stringify(input),
	encoding: 'utf8',
	env: { ...process.env, HOMEBOY_WP_CODEBOX_CORE_MODULE: '/missing/wp-codebox-core.mjs' },
});

assert.equal(diagnosticResult.status, 0, diagnosticResult.stderr);
assert.match(diagnosticResult.stderr, /HOMEBOY_WP_CODEBOX_CORE_MODULE could not be loaded; using bundled WP Codebox bench recipe builder/);
assert.match(diagnosticResult.stderr, /\/missing\/wp-codebox-core\.mjs/);

const invalidMountResult = spawnSync(process.execPath, [script], {
	cwd: path.join(__dirname, '..'),
	input: JSON.stringify({ options: { pluginSlug: 'fixture-plugin', mounts: [{ source: '/tmp/plugin', target: 'relative/path' }] } }),
	encoding: 'utf8',
	env: { ...process.env, HOMEBOY_WP_CODEBOX_CORE_MODULE: '' },
});

assert.notEqual(invalidMountResult.status, 0);
assert.match(invalidMountResult.stderr, /Recipe mount 0 requires an absolute target/);

console.log('wp-codebox bench recipe builder smoke passed');
