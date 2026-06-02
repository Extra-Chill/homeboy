const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const path = require('node:path');

const script = path.join(__dirname, '..', 'scripts', 'test', 'build-wp-codebox-phpunit-recipe.mjs');
const input = {
	wordpressVersion: '6.9',
	pluginSlug: 'fixture-plugin',
	selectedTestFile: 'tests/fixture-test.php',
	changedTestFiles: ['tests/fixture-test.php'],
	env: { FIXTURE: '1' },
	wpConfigDefines: { WP_DEBUG: true },
	dependencyMounts: ['/wordpress/wp-content/plugins/dependency-one'],
	multisite: true,
	mounts: [{ source: '/tmp/fixture-plugin', target: '/wordpress/wp-content/plugins/fixture-plugin' }],
};

const result = spawnSync(process.execPath, [script], {
	cwd: path.join(__dirname, '..'),
	input: JSON.stringify(input),
	encoding: 'utf8',
});

assert.equal(result.status, 0, result.stderr);
const recipe = JSON.parse(result.stdout);

assert.equal(recipe.schema, 'wp-codebox/workspace-recipe/v1');
assert.equal(recipe.inputs.mounts[0].mode, 'readwrite');
assert.deepEqual(recipe.workflow.steps, [{
	command: 'wordpress.phpunit',
	args: [
		'plugin-slug=fixture-plugin',
		'test-file=tests/fixture-test.php',
		'changed-tests-json=["tests/fixture-test.php"]',
		'env-json={"FIXTURE":"1"}',
		'wp-config-defines-json={"WP_DEBUG":true}',
		'autoload-file=/wp-codebox-vendor/autoload.php',
		'tests-dir=/wp-codebox-vendor/wp-phpunit/wp-phpunit',
		'dependency-mounts=/wordpress/wp-content/plugins/dependency-one',
		'multisite=1',
	],
}]);

console.log('wp-codebox phpunit recipe builder smoke passed');
