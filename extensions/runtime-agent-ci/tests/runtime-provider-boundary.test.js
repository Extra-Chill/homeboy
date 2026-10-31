'use strict';

require('./helpers/runtime-contract-constants-fixture.cjs');

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const { DEFAULT_RUNTIME_ID, resolveRuntimeProvider, runtimeIdFromOptions } = require('../lib/runtime-provider-resolver.cjs');
const { renderRuntimeWorkflowInputs } = require('../lib/runtime-workflow-inputs.cjs');
const { runtimeAgentCiRunnerSpec } = require('../provider-adapters');
const { runRuntimeSetup, runtimeSetupAdapter } = require('../../.github/scripts/runtime-agent-full-run/setup-runtime.cjs');
const { checkedOutPhpDependencyDirs, envPathIsInsideWorkspace, requiresWordPressDependencies, safeDependencySubdir, setupRuntime } = require('../../agent-runtimes/wp-codebox/lib/runtime-setup.cjs');

assert.equal(DEFAULT_RUNTIME_ID, 'local-shell');

assert.equal(
  runtimeAgentCiRunnerSpec({
    runtime: 'wp-codebox',
    ability: 'example/run-task',
    runtime_profile: 'example-runtime-ci',
    runtime_profiles: { 'example-runtime-ci': { id: 'example-runtime-ci', runtime_task_ability: 'example/run-task' } },
  }).executor.backend,
  'wp-codebox'
);

assert.equal(runtimeIdFromOptions({}, { RUNTIME: 'wp-codebox' }), 'wp-codebox');
assert.equal(runtimeIdFromOptions({}, { RUNTIME_PROVIDER: 'wp-codebox' }), DEFAULT_RUNTIME_ID);
assert.equal(runtimeIdFromOptions({}, { BACKEND: 'wp-codebox' }), DEFAULT_RUNTIME_ID);
const materializeCheckout = resolveRuntimeProvider(runtimeIdFromOptions({}, { RUNTIME: 'wp-codebox' }), { env: { RUNTIME: 'wp-codebox' } }).checkout;
assert.equal(materializeCheckout.repo, 'Automattic/wp-codebox');
assert.equal(materializeCheckout.target, '.ci/wp-codebox');

assert.equal(runtimeSetupAdapter({ manifest: { ci_materialization: {} } }), null);
assert.equal(runtimeSetupAdapter({ manifest: { ci_materialization: { requires_wordpress_dependencies: true } } }), null);

const overlayRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-runtime-overlay-'));
const overlayPackageRoot = path.join(overlayRoot, 'node_modules/@example/runtime-overlay');
fs.mkdirSync(path.join(overlayPackageRoot, 'lib'), { recursive: true });
fs.writeFileSync(path.join(overlayPackageRoot, 'package.json'), JSON.stringify({
  name: '@example/runtime-overlay',
  version: '1.0.0',
  homeboy: { agent_runtime_manifest: 'runtime.json' },
}, null, 2));
fs.writeFileSync(path.join(overlayPackageRoot, 'runtime.json'), JSON.stringify({
  schema: 'homeboy/agent-runtime-manifest/v1',
  id: 'overlay-runtime',
  version: '1.2.3',
  agent_task_executors: [{ id: 'overlay-executor', backend: 'local', invocation: { command: 'node', argv: ['node', '{{runtime_path}}/executor.js'] } }],
  workload_profiles: { default: { runtime_profile: 'default' } },
  workflow_input_projection: { adapter: { module: 'lib/workflow-inputs.cjs', export: 'renderInputs' } },
  ci_materialization: { setup_adapter: { module: 'lib/setup.cjs', export: 'setupOverlay' } },
}, null, 2));
fs.writeFileSync(path.join(overlayPackageRoot, 'lib/workflow-inputs.cjs'), `
'use strict';
exports.renderInputs = ({ runtime, profileId }) => ({
  runtime_requirements: { id: profileId, overlay: true },
  workflow_inputs: { runtime_source: runtime.source.source_path },
});
`);
fs.writeFileSync(path.join(overlayPackageRoot, 'lib/setup.cjs'), `
'use strict';
exports.setupOverlay = ({ runtime, run }) => run('overlay-setup', [runtime.source.source_path], { cwd: runtime.source.source_path });
`);

const overlayRuntime = resolveRuntimeProvider('overlay-runtime', {
  runtimePackages: ['@example/runtime-overlay'],
  env: { AGENT_RUNTIME_PACKAGE_BASE_PATHS: overlayRoot },
});
const expectedOverlayPackageRoot = fs.realpathSync(overlayPackageRoot);
assert.equal(overlayRuntime.source.source_path, expectedOverlayPackageRoot);
assert.deepEqual(overlayRuntime.source_state, {
  source_type: 'release',
  package: '@example/runtime-overlay',
  source_path: expectedOverlayPackageRoot,
  manifest_path: path.join(expectedOverlayPackageRoot, 'runtime.json'),
  manifest_version: '1.2.3',
});
assert.deepEqual(renderRuntimeWorkflowInputs({
  runtime: overlayRuntime,
  runtime_profile: 'default',
  workload_profile: 'default',
}).workflow_inputs, { runtime_source: expectedOverlayPackageRoot });
const overlaySetupCalls = [];
runtimeSetupAdapter(overlayRuntime)({ runtime: overlayRuntime, run: (...args) => overlaySetupCalls.push(args) });
assert.deepEqual(overlaySetupCalls, [['overlay-setup', [expectedOverlayPackageRoot], { cwd: expectedOverlayPackageRoot }]]);

const localManifestRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-runtime-local-manifest-'));
const localManifestPath = path.join(localManifestRoot, 'runtime.json');
fs.writeFileSync(localManifestPath, JSON.stringify({
  schema: 'homeboy/agent-runtime-manifest/v1',
  id: 'local-manifest-runtime',
  agent_task_executors: [{ id: 'local-manifest-executor', backend: 'local', invocation: { command: 'node', argv: ['node', '{{runtime_path}}/executor.js'] } }],
}, null, 2));
const localManifestRuntime = resolveRuntimeProvider('local-manifest-runtime', {
  runtimeManifests: [localManifestPath],
  env: {},
});
assert.deepEqual(localManifestRuntime.source_state, {
  source_type: 'local',
  source_path: localManifestRoot,
  manifest_path: localManifestPath,
});

const linkedRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-runtime-linked-'));
const linkedSourceRoot = path.join(linkedRoot, 'source-runtime');
const linkedNodeModulesRoot = path.join(linkedRoot, 'consumer/node_modules/@example');
const linkedPackageRoot = path.join(linkedNodeModulesRoot, 'runtime-linked');
fs.mkdirSync(linkedSourceRoot, { recursive: true });
fs.mkdirSync(linkedNodeModulesRoot, { recursive: true });
fs.writeFileSync(path.join(linkedSourceRoot, 'package.json'), JSON.stringify({
  name: '@example/runtime-linked',
  version: '1.0.0',
  homeboy: { agent_runtime_manifest: 'runtime.json' },
}, null, 2));
fs.writeFileSync(path.join(linkedSourceRoot, 'runtime.json'), JSON.stringify({
  schema: 'homeboy/agent-runtime-manifest/v1',
  id: 'linked-runtime',
  version: '2.0.0',
  agent_task_executors: [{ id: 'linked-executor', backend: 'local', invocation: { command: 'node', argv: ['node', '{{runtime_path}}/executor.js'] } }],
}, null, 2));
fs.symlinkSync(linkedSourceRoot, linkedPackageRoot, 'dir');
const linkedRuntime = resolveRuntimeProvider('linked-runtime', {
  runtimePackages: ['@example/runtime-linked'],
  env: { AGENT_RUNTIME_PACKAGE_BASE_PATHS: path.join(linkedRoot, 'consumer') },
});
assert.deepEqual(linkedRuntime.source_state, {
  source_type: 'linked',
  package: '@example/runtime-linked',
  source_path: fs.realpathSync(linkedSourceRoot),
  manifest_path: path.join(fs.realpathSync(linkedSourceRoot), 'runtime.json'),
  manifest_version: '2.0.0',
});

const setupCalls = [];
runRuntimeSetup({ manifest: { ci_materialization: {} } }, {
  phase: 'before_commands',
  workspace: '/tmp/workspace',
  env: {
    PROFILE: 'wordpress-ci',
    RUNTIME_PROFILES: JSON.stringify({ 'wordpress-ci': { requires_wordpress_dependencies: true } }),
  },
  run: (...args) => setupCalls.push(args),
});
assert.deepEqual(setupCalls, []);

assert.equal(requiresWordPressDependencies({}), true);
assert.equal(
  requiresWordPressDependencies({
    PROFILE: 'wordpress-ci',
    RUNTIME_PROFILES: JSON.stringify({ 'wordpress-ci': { requires_wordpress_dependencies: true } }),
  }),
  true
);
assert.equal(
  requiresWordPressDependencies({
    PROFILE: 'generic-ci',
    RUNTIME_PROFILES: JSON.stringify({ 'generic-ci': { runtime_task_ability: 'example/run-task', requires_wordpress_dependencies: false } }),
  }),
  false
);

const codeboxSetupCalls = [];
setupRuntime({
  phase: 'before_commands',
  workspace: '/tmp/workspace',
  env: {},
  run: (...args) => codeboxSetupCalls.push(args),
});
assert.deepEqual(codeboxSetupCalls.map(([command, args, options]) => ({ command, args, cwd: options.cwd })), [
  {
    command: 'composer',
    args: ['install', '--no-interaction', '--no-progress', '--prefer-dist'],
    cwd: '/tmp/workspace/.ci/homeboy-extensions/wordpress',
  },
  {
    command: 'npm',
    args: ['install'],
    cwd: '/tmp/workspace/.ci/homeboy-extensions/wordpress',
  },
]);

const dependencyWorkspace = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-dependencies-'));
const providerComposerJson = path.join(dependencyWorkspace, '.ci/ai-provider-for-openai/composer.json');
const runtimeDependencyComposerJson = path.join(dependencyWorkspace, '.ci/dependencies/runtime-helper-plugin/plugin/composer.json');
const arbitraryComposerJson = path.join(dependencyWorkspace, '.ci/arbitrary/composer.json');
fs.mkdirSync(path.dirname(providerComposerJson), { recursive: true });
fs.mkdirSync(path.dirname(runtimeDependencyComposerJson), { recursive: true });
fs.mkdirSync(path.dirname(arbitraryComposerJson), { recursive: true });
fs.writeFileSync(providerComposerJson, '{}\n');
fs.writeFileSync(runtimeDependencyComposerJson, '{}\n');
fs.writeFileSync(arbitraryComposerJson, '{}\n');

assert.deepEqual(checkedOutPhpDependencyDirs({
  workspace: dependencyWorkspace,
  env: {
    PROVIDER_PLUGIN: JSON.stringify({ repo: 'WordPress/ai-provider-for-openai', path: '.' }),
    RUNTIME_DEPENDENCIES: JSON.stringify([{ repo: 'Extra-Chill/runtime-helper-plugin', ref: 'main', target: '.ci/dependencies/runtime-helper-plugin', path: 'plugin' }]),
  },
}), [
  path.join(dependencyWorkspace, '.ci/dependencies/runtime-helper-plugin/plugin'),
  path.join(dependencyWorkspace, '.ci/ai-provider-for-openai'),
]);
assert.throws(
  () => safeDependencySubdir('.ci/dependencies/runtime-helper-plugin', '../escape', dependencyWorkspace),
  /Dependency subdirectory must resolve inside dependency checkout/
);

const installedDependencyDirs = [];
setupRuntime({
  phase: 'after_commands',
  workspace: dependencyWorkspace,
  env: {
    PROVIDER_PLUGIN: JSON.stringify({ repo: 'WordPress/ai-provider-for-openai', path: '.' }),
    RUNTIME_DEPENDENCIES: JSON.stringify([{ repo: 'Extra-Chill/runtime-helper-plugin', ref: 'main', target: '.ci/dependencies/runtime-helper-plugin', path: 'plugin' }]),
  },
  run: (command, args, options) => installedDependencyDirs.push({ command, args, cwd: options.cwd }),
});
assert.deepEqual(installedDependencyDirs.map((call) => call.cwd), [
  path.join(dependencyWorkspace, '.ci/dependencies/runtime-helper-plugin/plugin'),
  path.join(dependencyWorkspace, '.ci/ai-provider-for-openai'),
]);

const runtimeWorkspace = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-runtime-'));
const githubEnvPath = path.join(runtimeWorkspace, 'github-env');
const builtCliPath = path.join(runtimeWorkspace, '.ci/wp-codebox/packages/cli/dist/index.js');
const builtContractsPath = path.join(runtimeWorkspace, '.ci/wp-codebox/packages/runtime-core/dist/contracts.js');
fs.mkdirSync(path.dirname(builtCliPath), { recursive: true });
fs.mkdirSync(path.dirname(builtContractsPath), { recursive: true });
fs.writeFileSync(builtCliPath, '#!/usr/bin/env node\n');
fs.writeFileSync(builtContractsPath, 'export {};\n');

assert.equal(envPathIsInsideWorkspace(path.join(runtimeWorkspace, '.ci/wp-codebox/packages/cli/dist/index.js'), runtimeWorkspace), true);
assert.equal(envPathIsInsideWorkspace('/home/chubes/Developer/wp-codebox@main-fuzz-proof-20260625/packages/cli/dist/index.js', runtimeWorkspace), false);

setupRuntime({
  phase: 'after_commands',
  workspace: runtimeWorkspace,
  env: {
    GITHUB_ENV: githubEnvPath,
    HOMEBOY_WP_CODEBOX_BIN: '/home/chubes/Developer/wp-codebox@main-fuzz-proof-20260625/packages/cli/dist/index.js',
    HOMEBOY_WP_CODEBOX_CORE_MODULE: '/home/chubes/Developer/wp-codebox@main-fuzz-proof-20260625/packages/runtime-core/dist/index.js',
  },
  run: () => {},
});
const githubEnv = fs.readFileSync(githubEnvPath, 'utf8');
assert.match(githubEnv, new RegExp(`HOMEBOY_WP_CODEBOX_BIN=${escapeRegExp(builtCliPath)}`));
assert.match(githubEnv, new RegExp(`HOMEBOY_WP_CODEBOX_CORE_MODULE=${escapeRegExp(builtContractsPath)}`));

fs.writeFileSync(githubEnvPath, '');
setupRuntime({
  phase: 'after_commands',
  workspace: runtimeWorkspace,
  env: {
    GITHUB_ENV: githubEnvPath,
    HOMEBOY_WP_CODEBOX_BIN: builtCliPath,
    HOMEBOY_WP_CODEBOX_CORE_MODULE: builtContractsPath,
  },
  run: () => {},
});
assert.equal(fs.readFileSync(githubEnvPath, 'utf8'), '');

function escapeRegExp(value) {
  return String(value).replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

process.stdout.write('Runtime provider boundary passed\n');
