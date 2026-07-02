'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const { DEFAULT_RUNTIME_ID, resolveRuntimeProvider, runtimeIdFromOptions } = require('../lib/runtime-provider-resolver.cjs');
const { runtimeAgentCiRunnerSpec } = require('..');
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

// Regression guard: materialize-dependencies resolves the runtime id via
// runtimeIdFromOptions while setup-runtime resolves it via
// RUNTIME || RUNTIME_PROVIDER || BACKEND. When a consumer sets only
// RUNTIME_PROVIDER (or BACKEND), both must resolve the SAME runtime so the
// runtime provider repo is checked out into .ci/<runtime> before its npm
// setup/build commands run there. Honoring only RUNTIME/RUNTIME_ID here makes
// materialize fall back to DEFAULT_RUNTIME_ID (local-shell, no checkout), so
// setup-runtime later spawns `npm install` in a never-created .ci/wp-codebox
// and fails with a missing-cwd ENOENT.
for (const env of [{ RUNTIME_PROVIDER: 'wp-codebox' }, { BACKEND: 'wp-codebox' }]) {
  const materializeRuntimeId = runtimeIdFromOptions({}, env);
  const setupRuntimeId = env.RUNTIME || env.RUNTIME_PROVIDER || env.BACKEND || DEFAULT_RUNTIME_ID;
  assert.equal(
    materializeRuntimeId,
    setupRuntimeId,
    `materialize and setup-runtime must resolve the same runtime for ${JSON.stringify(env)}`
  );
  assert.equal(materializeRuntimeId, 'wp-codebox');
  const materializeCheckout = resolveRuntimeProvider(materializeRuntimeId, { env }).checkout;
  assert.equal(materializeCheckout.repo, 'Automattic/wp-codebox');
  assert.equal(materializeCheckout.target, '.ci/wp-codebox');
}

assert.equal(runtimeSetupAdapter({ manifest: { ci_materialization: {} } }), null);
assert.equal(runtimeSetupAdapter({ manifest: { ci_materialization: { requires_wordpress_dependencies: true } } }), null);

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
