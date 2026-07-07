'use strict';

require('../../../runtime-agent-ci/tests/helpers/runtime-contract-constants-fixture.cjs');

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE ||= path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'wp-codebox-core-runtime-contract.cjs');

const {
	runtimeDescriptorSupportsCommand,
	wpCodeboxBin,
	wpCodeboxBinaryDiagnostic,
	wpCodeboxProviderPluginPathsFromEnv,
	wpCodeboxResolveCommand,
	wpCodeboxSupportsRunAgentTaskCommand,
} = require('..');

const runtimeRoot = path.join(__dirname, '..');
const runnerSource = fs.readFileSync(path.join(runtimeRoot, 'scripts', 'agent', 'homeboy-wp-codebox-task-runner.cjs'), 'utf8');

assert.doesNotMatch(runnerSource, /HOMEBOY_AGENT_RUNTIME_PROVIDER_PLUGIN_PATHS|WP_CODEBOX_PROVIDER_PLUGIN_PATHS/);
assert.doesNotMatch(runnerSource, /packages\/cli\/dist|HOMEBOY_WP_CODEBOX_RUNTIME_COMPONENT|HOMEBOY_WP_CODEBOX_INSTALL_DIR/);

const descriptorSource = fs.readFileSync(path.join(runtimeRoot, 'lib', 'wp-codebox-adapter-descriptor.js'), 'utf8');
assert.match(descriptorSource, /runtime', 'descriptor', '--json/);
assert.doesNotMatch(descriptorSource, /run-agent-task', '--help/);

const manifest = JSON.parse(fs.readFileSync(path.join(runtimeRoot, 'wp-codebox.json'), 'utf8'));
assert.equal(manifest.component_path_defaults, undefined);
const runtimePreflightCheck = manifest.agent_task_executors[0].runtime_contract.preflight_checks.find((check) => check.id === 'wp-codebox.provider_plugin.runtime_package_shadow');
assert.equal(runtimePreflightCheck.enforcement, 'error');
assert.deepEqual(runtimePreflightCheck.target.component.metadata_equals, { loadAs: 'plugin' });
assert.deepEqual(runtimePreflightCheck.target.component.metadata_any_equals, { activate: true });
assert.deepEqual(runtimePreflightCheck.path_probes.exists.map((probe) => probe.path), ['vendor/automattic/php-ai-client']);
const providerPreflight = manifest.agent_task_executors[0].config_preflights.find((preflight) => preflight.id === 'recipe-command-compatibility');
assert.equal(providerPreflight.label, 'WP Codebox recipe command compatibility');
assert.deepEqual(providerPreflight.required_values.keys, ['command']);
assert.equal(providerPreflight.supported_values.scoped_keys.includes('supported_recipe_commands'), true);
assert.deepEqual(providerPreflight.reference_key_contains, ['recipe']);
assert.equal(providerPreflight.binary_probe.path_env.includes('HOMEBOY_WP_CODEBOX_BIN'), true);

const executorSource = fs.readFileSync(path.join(runtimeRoot, 'lib', 'codebox-agent-task-executor.js'), 'utf8');
assert.doesNotMatch(executorSource, /function defaultRuntimeRequirements/);
assert.doesNotMatch(executorSource, /slug: 'agents-api'/);
assert.doesNotMatch(executorSource, /pluginFile: 'agents-api\/agents-api\.php'/);
assert.doesNotMatch(executorSource, /function typedArtifactNameFromDeclaration/);
assert.match(executorSource, /artifactNameFromDeclaration\(declaration\)/);

assert.deepEqual(wpCodeboxProviderPluginPathsFromEnv({
  WP_CODEBOX_PROVIDER_PLUGIN_PATHS: JSON.stringify(['/tmp/provider-a', '/tmp/provider-b']),
}), ['/tmp/provider-a', '/tmp/provider-b']);
assert.deepEqual(wpCodeboxProviderPluginPathsFromEnv({
  HOMEBOY_AGENT_RUNTIME_PROVIDER_PLUGIN_PATH: ['/tmp/provider-a', '/tmp/provider-b'].join(path.delimiter),
  WP_CODEBOX_PROVIDER_PLUGIN_PATHS: JSON.stringify(['/tmp/provider-c']),
}), ['/tmp/provider-a', '/tmp/provider-b']);

assert.equal(wpCodeboxBinaryDiagnostic('').class, 'wp-codebox.config.missing_binary');
assert.equal(wpCodeboxBinaryDiagnostic('wp-codebox'), null);
assert.deepEqual(wpCodeboxResolveCommand('/tmp/wp-codebox.cjs', ['run-agent-task']).args, ['/tmp/wp-codebox.cjs', 'run-agent-task']);
assert.equal(wpCodeboxBin({
	env: {
		HOMEBOY_WP_CODEBOX_BIN: '/tmp/env-wp-codebox',
		HOMEBOY_WP_CODEBOX_INSTALL_DIR: path.join(__dirname, '.missing-wp-codebox-install'),
	},
	settings: { wp_codebox_bin: '/tmp/settings-wp-codebox' },
}), '/tmp/env-wp-codebox');

assert.equal(runtimeDescriptorSupportsCommand({ commands: { 'run-agent-task': true } }, 'run-agent-task'), true);
assert.equal(runtimeDescriptorSupportsCommand({ runtime: { tasks: ['run-agent-task'] } }, 'run-agent-task'), true);
assert.equal(runtimeDescriptorSupportsCommand({ capabilities: ['wp-codebox/run-agent-task'] }, 'run-agent-task'), true);
assert.equal(wpCodeboxSupportsRunAgentTaskCommand({
	bin: '/tmp/wp-codebox',
	env: {},
	spawnSync(command, args) {
		assert.equal(command, '/tmp/wp-codebox');
		assert.deepEqual(args, ['runtime', 'descriptor', '--json']);
		return {
			status: 0,
			stdout: JSON.stringify({ commands: [{ command: 'run-agent-task' }] }),
		};
	},
}), true);

process.stdout.write('WP Codebox adapter boundary passed\n');
