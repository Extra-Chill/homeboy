'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const repoRoot = path.join(__dirname, '..', '..');
const { DEFAULT_RUNTIME_ID } = require('../lib/runtime-provider-resolver.cjs');
const { runtimeAgentCiRunnerSpec } = require('..');
const { requiresWordPressDependencies } = require('../../.github/scripts/runtime-agent-full-run/setup-runtime.cjs');

assert.equal(DEFAULT_RUNTIME_ID, 'local-shell');

const runtimeAgentCiWorkflow = fs.readFileSync(path.join(repoRoot, '.github/workflows/runtime-agent-ci.yml'), 'utf8');
assert.match(runtimeAgentCiWorkflow, /runtime_provider:[\s\S]*?default: ''/);
assert.doesNotMatch(runtimeAgentCiWorkflow, /\|\| 'wp-codebox'/);

const runAgentLoop = fs.readFileSync(path.join(repoRoot, 'runtime-agent-ci/scripts/run-agent-loop.cjs'), 'utf8');
assert.doesNotMatch(runAgentLoop, /\|\| 'wp-codebox'/);
assert.match(runAgentLoop, /DEFAULT_RUNTIME_ID/);

const genericAdapters = fs.readFileSync(path.join(repoRoot, 'scripts/lib/test-result-adapters.sh'), 'utf8');
assert.doesNotMatch(genericAdapters, /wp-codebox\/test-results\/v1/);
assert.doesNotMatch(genericAdapters, /wp-codebox-json/);

const wordpressAdapters = fs.readFileSync(path.join(repoRoot, 'wordpress/scripts/lib/test-result-adapters.sh'), 'utf8');
assert.doesNotMatch(wordpressAdapters, /wp-codebox\/test-results\/v1/);
assert.doesNotMatch(wordpressAdapters, /wp-codebox-json/);

const wpCodeboxAdapters = fs.readFileSync(path.join(repoRoot, 'agent-runtimes/wp-codebox/scripts/lib/test-result-adapters.sh'), 'utf8');
assert.match(wpCodeboxAdapters, /wp-codebox\/test-results\/v1/);

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
