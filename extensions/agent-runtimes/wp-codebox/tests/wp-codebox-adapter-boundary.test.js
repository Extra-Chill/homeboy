'use strict';

require('../../../runtime-agent-ci/tests/helpers/runtime-contract-constants-fixture.cjs');

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');

process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE ||= path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'wp-codebox-core-runtime-contract.cjs');

const {
	runtimeDescriptorSupportsCommand,
	wpCodeboxBin,
	wpCodeboxBinaryDiagnostic,
	wpCodeboxProviderPluginPathsFromEnv,
	wpCodeboxResolveCommand,
	wpCodeboxSupportsRunAgentTaskCommand,
} = require('..');
const { setupExtensionBrowserCache } = require('../lib/runtime-setup.cjs');
const { runtimeEnvFromHost } = require('../../../runtime-agent-ci/lib/full-run-config.cjs');

const runtimeRoot = path.join(__dirname, '..');
const descriptorSource = fs.readFileSync(path.join(runtimeRoot, 'lib', 'wp-codebox-adapter-descriptor.js'), 'utf8');
assert.match(descriptorSource, /preflightWpCodeboxRuntime/);
assert.match(descriptorSource, /preflightWpCodeboxCommand/);
assert.match(descriptorSource, /probeWpCodeboxRuntimeDescriptor/);
assert.doesNotMatch(descriptorSource, /run-agent-task', '--help/);

const manifest = JSON.parse(fs.readFileSync(path.join(runtimeRoot, 'wp-codebox.json'), 'utf8'));
assert.equal(manifest.component_path_defaults, undefined);
assert.equal(manifest.agent_task_executors[0].cli.default_ai_disclosure, 'OpenCode');
assert.equal(Object.hasOwn(manifest.agent_task_executors[0].cli, 'profiles'), false);
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
	bin: process.execPath,
	env: {},
	spawnSync(command, args) {
		assert.equal(command, process.execPath);
		if (args.includes('--version')) {
			return { status: 0, stdout: '0.21.0' };
		}
		assert.deepEqual(args, ['runtime', 'descriptor', '--json']);
		return {
			status: 0,
			stdout: JSON.stringify({
				schema: 'wp-codebox/runtime-descriptor/v1',
				readiness: { status: 'available', browserRuntime: { status: 'ready' } },
				contractManifest: { schemas: { runtimeBoundary: { browserContainedSiteOpen: 'wp-codebox/browser-contained-site-open/v1' } } },
				commands: [{ command: 'run-agent-task' }],
			}),
		};
	},
}), true);

const setupWorkspace = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-playwright-cache-contract-'));
const setupUtility = path.join(setupWorkspace, '.ci', 'homeboy-extensions', 'nodejs', 'scripts', 'browser');
fs.mkdirSync(setupUtility, { recursive: true });
fs.writeFileSync(path.join(setupUtility, 'playwright.sh'), '#!/usr/bin/env bash\n');
const runtimeEnvFile = path.join(setupWorkspace, 'runtime.env');
const invokedActions = [];
setupExtensionBrowserCache({
  workspace: setupWorkspace,
  env: { GITHUB_ENV: runtimeEnvFile },
  spawn(_command, args) {
    invokedActions.push(args.slice(1));
    return args[1] === 'setup'
      ? { status: 0, stdout: '', stderr: '' }
      : { status: 0, stdout: JSON.stringify({ package: { state: 'ready' }, chromium: { state: 'ready' }, browser_cache_dir: '/runner/cache/homeboy/nodejs-playwright/browsers' }), stderr: '' };
  },
});
assert.deepEqual(invokedActions, [['setup'], ['status', '--json']]);
assert.equal(fs.readFileSync(runtimeEnvFile, 'utf8'), 'HOMEBOY_RUNTIME_ENV_PLAYWRIGHT_BROWSERS_PATH=/runner/cache/homeboy/nodejs-playwright/browsers\n');
assert.deepEqual(runtimeEnvFromHost({ HOMEBOY_RUNTIME_ENV_PLAYWRIGHT_BROWSERS_PATH: '/runner/cache/homeboy/nodejs-playwright/browsers', HOMEBOY_RUNTIME_ENV_TOKEN: 'secret-value' }), { PLAYWRIGHT_BROWSERS_PATH: '/runner/cache/homeboy/nodejs-playwright/browsers' });
fs.rmSync(setupWorkspace, { recursive: true, force: true });

process.stdout.write('WP Codebox adapter boundary passed\n');
