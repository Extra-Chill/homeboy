'use strict';

const assert = require('node:assert/strict');

const { DEFAULT_RUNTIME_ID } = require('../lib/runtime-provider-resolver.cjs');
const { runtimeAgentCiRunnerSpec } = require('..');
const { requiresWordPressDependencies } = require('../../.github/scripts/runtime-agent-full-run/setup-runtime.cjs');

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

assert.equal(requiresWordPressDependencies({ manifest: { ci_materialization: {} } }, {}), false);
assert.equal(requiresWordPressDependencies({ manifest: { ci_materialization: { requires_wordpress_dependencies: true } } }, {}), true);
assert.equal(
  requiresWordPressDependencies({ manifest: { ci_materialization: {} } }, {
    PROFILE: 'wordpress-ci',
    RUNTIME_PROFILES: JSON.stringify({ 'wordpress-ci': { requires_wordpress_dependencies: true } }),
  }),
  true
);
assert.equal(
  requiresWordPressDependencies({ manifest: { ci_materialization: {} } }, {
    PROFILE: 'generic-ci',
    RUNTIME_PROFILES: JSON.stringify({ 'generic-ci': { runtime_task_ability: 'example/run-task' } }),
  }),
  false
);

process.stdout.write('Runtime provider boundary passed\n');
