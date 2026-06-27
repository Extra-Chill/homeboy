'use strict';

const assert = require('node:assert/strict');

const { DEFAULT_RUNTIME_ID } = require('../lib/runtime-provider-resolver.cjs');
const { runtimeAgentCiRunnerSpec } = require('..');
const { runRuntimeSetup, runtimeSetupAdapter } = require('../../.github/scripts/runtime-agent-full-run/setup-runtime.cjs');
const { requiresWordPressDependencies, setupRuntime } = require('../../agent-runtimes/wp-codebox/lib/runtime-setup.cjs');

assert.equal(DEFAULT_RUNTIME_ID, 'local-shell');

assert.equal(
  runtimeAgentCiRunnerSpec({
    runtime: 'wp-codebox',
    ability: 'example/run-task',
    runtimeProfile: 'example-runtime-ci',
    runtimeProfiles: { 'example-runtime-ci': { id: 'example-runtime-ci', runtime_task_ability: 'example/run-task' } },
  }).executor.backend,
  'codebox'
);

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

let installedCheckedOutDependencies = false;
setupRuntime({
  phase: 'after_commands',
  workspace: '/tmp/workspace',
  env: {},
  run: () => {},
  installCheckedOutPhpDependencies: (workspace) => {
    installedCheckedOutDependencies = workspace === '/tmp/workspace';
  },
});
assert.equal(installedCheckedOutDependencies, true);

process.stdout.write('Runtime provider boundary passed\n');
