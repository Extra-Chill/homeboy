const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const script = path.join(__dirname, '..', 'scripts', 'bench', 'build-wp-codebox-bench-recipe.mjs');
const readyScript = path.join(__dirname, '..', 'scripts', 'build', 'check-wp-codebox-runtime-core.mjs');
const fixtureCoreModule = path.join(__dirname, 'fixtures', 'wp-codebox-core-recipe-builder.mjs');
const missingBenchBuilderModule = path.join(__dirname, 'fixtures', 'wp-codebox-core-missing-bench-builder.mjs');
const installRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-install-'));
const cachedCoreModule = path.join(installRoot, 'source', 'packages', 'runtime-core', 'dist', 'index.js');
fs.mkdirSync(path.dirname(cachedCoreModule), { recursive: true });
fs.copyFileSync(fixtureCoreModule, cachedCoreModule);

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
		workloads: [
			{ id: 'fixture-workload', steps: [{ type: 'php', code: 'return [];' }] },
			{ id: 'other-workload', steps: [{ type: 'php', code: 'return [];' }] },
		],
		lifecycle: { before: ['fixture'] },
		resetPolicy: { mode: 'snapshot' },
		pluginRuntime: {
			url: 'https://example.com/runtime.zip',
			sha256: 'abc123',
			setup: [{ command: 'wordpress.wp-cli', args: ['command=option update fixture_bootstrap yes'] }],
		},
		fixtureProfile: {
			siteSeeds: [{
				type: 'fixture',
				name: 'generic-fixture-content',
				source: 'fixtures/content.json',
				format: 'json',
				scopes: { posts: { slugs: ['home'] }, options: { names: ['blogname'] } },
			}],
		},
		diagnosticsCapture: true,
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
assert.deepEqual(recipe.inputs.workloads.map((workload) => workload.id), ['fixture-workload', 'other-workload']);
assert.deepEqual(recipe.inputs.pluginRuntime, input.options.pluginRuntime);
assert.deepEqual(recipe.inputs.siteSeeds, [{
	type: 'fixture',
	name: 'generic-fixture-content',
	source: 'fixtures/content.json',
	format: 'json',
	scopes: { posts: { slugs: ['home'] }, options: { names: ['blogname'] } },
}]);
assert.deepEqual(recipe.workflow.steps, [{
	command: 'fixture.wordpress.bench',
	args: [
		'plugin-slug=fixture-plugin',
		'lifecycle-json={"before":["fixture"]}',
		'reset-policy-json={"mode":"snapshot"}',
	],
	diagnostics: { capture: ['queries', 'errors'] },
}]);

const filteredResult = spawnSync(process.execPath, [script], {
	cwd: path.join(__dirname, '..'),
	input: JSON.stringify(input),
	encoding: 'utf8',
	env: { ...process.env, HOMEBOY_WP_CODEBOX_CORE_MODULE: fixtureCoreModule, HOMEBOY_BENCH_SCENARIOS: 'fixture-workload' },
});

assert.equal(filteredResult.status, 0, filteredResult.stderr);
const filteredRecipe = JSON.parse(filteredResult.stdout);
assert.deepEqual(filteredRecipe.inputs.workloads.map((workload) => workload.id), ['fixture-workload']);
assert.deepEqual(filteredRecipe.inputs.scenarioIds, ['fixture-workload']);

const checkpointResult = spawnSync(process.execPath, [script], {
	cwd: path.join(__dirname, '..'),
	input: JSON.stringify({
		options: {
			...input.options,
			caseIsolation: {
				checkpoints: true,
				checkpointName: 'fixture-case-baseline',
			},
		},
	}),
	encoding: 'utf8',
	env: { ...process.env, HOMEBOY_WP_CODEBOX_CORE_MODULE: fixtureCoreModule },
});

assert.equal(checkpointResult.status, 0, checkpointResult.stderr);
const checkpointRecipe = JSON.parse(checkpointResult.stdout);
assert.deepEqual(checkpointRecipe.workflow.steps, [
	{ command: 'wp-codebox.checkpoint-create', args: ['name=fixture-case-baseline'] },
	{ command: 'wp-codebox.checkpoint-list', args: [] },
	{ command: 'wp-codebox.checkpoint-restore', args: ['name=fixture-case-baseline'] },
	{
		command: 'fixture.wordpress.bench',
		args: [
			'plugin-slug=fixture-plugin',
			'lifecycle-json={"before":["fixture"]}',
			'reset-policy-json={"mode":"snapshot"}',
			'scenario-ids-json=["fixture-workload"]',
		],
	},
	{ command: 'wp-codebox.checkpoint-list', args: [] },
	{ command: 'wp-codebox.checkpoint-restore', args: ['name=fixture-case-baseline'] },
	{
		command: 'fixture.wordpress.bench',
		args: [
			'plugin-slug=fixture-plugin',
			'lifecycle-json={"before":["fixture"]}',
			'reset-policy-json={"mode":"snapshot"}',
			'scenario-ids-json=["other-workload"]',
		],
	},
	{ command: 'wp-codebox.checkpoint-list', args: [] },
]);

const cacheDiscoveryResult = spawnSync(process.execPath, [script], {
	cwd: path.join(__dirname, '..'),
	input: JSON.stringify(input),
	encoding: 'utf8',
	env: { ...process.env, HOMEBOY_WP_CODEBOX_CORE_MODULE: '', HOMEBOY_WP_CODEBOX_INSTALL_DIR: installRoot },
});

assert.equal(cacheDiscoveryResult.status, 0, cacheDiscoveryResult.stderr);
const cacheDiscoveredRecipe = JSON.parse(cacheDiscoveryResult.stdout);
assert.equal(cacheDiscoveredRecipe.schema, 'wp-codebox/workspace-recipe/v1');

const readyResult = spawnSync(process.execPath, [readyScript], {
	cwd: path.join(__dirname, '..'),
	encoding: 'utf8',
	env: { ...process.env, HOMEBOY_WP_CODEBOX_CORE_MODULE: '', HOMEBOY_WP_CODEBOX_INSTALL_DIR: installRoot },
});

assert.equal(readyResult.status, 0, readyResult.stderr);
assert.match(readyResult.stdout, /WP Codebox runtime core ready:/);

const diagnosticResult = spawnSync(process.execPath, [script], {
	cwd: path.join(__dirname, '..'),
	input: JSON.stringify(input),
	encoding: 'utf8',
	env: { ...process.env, HOMEBOY_WP_CODEBOX_CORE_MODULE: '/missing/wp-codebox-core.mjs' },
});

assert.notEqual(diagnosticResult.status, 0);
assert.match(diagnosticResult.stderr, /WP Codebox recipe builder export buildWordPressBenchRecipe is unavailable/);
assert.match(diagnosticResult.stderr, /--setting wp_codebox_core_module=\/path\/to\/wp-codebox\/packages\/runtime-core\/dist\/recipe-builders\.js/);
assert.match(diagnosticResult.stderr, /HOMEBOY_WP_CODEBOX_CORE_MODULE/);
assert.match(diagnosticResult.stderr, /HOMEBOY_WP_CODEBOX_BIN \/ wp_codebox_bin/);
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
