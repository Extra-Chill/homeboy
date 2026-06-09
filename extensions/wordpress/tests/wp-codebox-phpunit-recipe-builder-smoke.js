const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const script = path.join(__dirname, '..', 'scripts', 'test', 'build-wp-codebox-phpunit-recipe.mjs');
const fixtureCoreModule = path.join(__dirname, 'fixtures', 'wp-codebox-core-recipe-builder.mjs');
const input = {
	wordpressVersion: '6.9',
	pluginSlug: 'fixture-plugin',
	selectedTestFile: 'tests/fixture-test.php',
	changedTestFiles: ['tests/fixture-test.php'],
	phpunitArgs: ['--filter', 'FixtureTest::test_selected'],
	env: { FIXTURE: '1' },
	wpConfigDefines: { WP_DEBUG: true },
	bootstrapFiles: ['tests/bootstrap-fragment.php'],
	bootstrapMode: 'project',
	projectBootstrap: 'tests/legacy/bootstrap.php',
	dependencyMounts: ['/wordpress/wp-content/plugins/dependency-one'],
	multisite: true,
	mounts: [{ source: '/tmp/fixture-plugin', target: '/wordpress/wp-content/plugins/fixture-plugin' }],
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
assert.equal(recipe.inputs.mounts[0].mode, 'readwrite');
assert.deepEqual(recipe.workflow.steps, [{
	command: 'fixture.wordpress.phpunit',
	args: [
		'plugin-slug=fixture-plugin',
		'phpunit-args-json=["--filter","FixtureTest::test_selected"]',
		'bootstrap-mode=project',
		'project-bootstrap=tests/legacy/bootstrap.php',
	],
}]);

const diagnosticResult = spawnSync(process.execPath, [script], {
	cwd: path.join(__dirname, '..'),
	input: JSON.stringify(input),
	encoding: 'utf8',
	env: { ...process.env, HOMEBOY_WP_CODEBOX_CORE_MODULE: '/missing/wp-codebox-core.mjs' },
});

assert.notEqual(diagnosticResult.status, 0);
assert.match(diagnosticResult.stderr, /WP Codebox recipe builder export buildWordPressPhpunitRecipe is unavailable/);
assert.match(diagnosticResult.stderr, /--setting wp_codebox_core_module=\/path\/to\/wp-codebox\/packages\/runtime-core\/dist\/index\.js/);
assert.match(diagnosticResult.stderr, /no longer falls back to bundled WP Codebox recipe builders/);
assert.match(diagnosticResult.stderr, /HOMEBOY_WP_CODEBOX_CORE_MODULE/);
assert.match(diagnosticResult.stderr, /\/missing\/wp-codebox-core\.mjs/);

const workspaceRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-workspace-'));
const discoveredModule = path.join(workspaceRoot, 'wp-codebox', 'packages', 'runtime-core', 'dist', 'index.js');
fs.mkdirSync(path.dirname(discoveredModule), { recursive: true });
fs.copyFileSync(fixtureCoreModule, discoveredModule);

const discoveredResult = spawnSync(process.execPath, [script], {
	cwd: path.join(__dirname, '..'),
	input: JSON.stringify(input),
	encoding: 'utf8',
	env: {
		...process.env,
		HOMEBOY_WORKSPACE_ROOT: workspaceRoot,
		HOMEBOY_WP_CODEBOX_CORE_MODULE: '',
	},
});

assert.equal(discoveredResult.status, 0, discoveredResult.stderr);
assert.equal(JSON.parse(discoveredResult.stdout).schema, 'wp-codebox/workspace-recipe/v1');

console.log('wp-codebox phpunit recipe builder smoke passed');
