#!/usr/bin/env node
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const require = createRequire(import.meta.url);
const before = new Set(Object.keys(require.cache));

const runtimeAgentCi = require(path.join(repoRoot, 'runtime-agent-ci/generic-orchestration'));
const genericWorkflow = require(path.join(repoRoot, 'runtime-agent-ci/lib/generic-fanout-reconcile-workflow'));
const runner = require(path.join(repoRoot, 'runtime-agent-ci/lib/fanout-reconcile-runner'));

assert.equal(typeof runtimeAgentCi.createGenericFanoutReconcilePlan, 'function');
assert.equal(typeof runtimeAgentCi.createGenericFanoutReconcileResult, 'function');
assert.equal(typeof runtimeAgentCi.validateControllerLoopProof, 'function');
assert.equal(typeof runtimeAgentCi.createFanoutReconcilePlan, 'function');
assert.equal(runtimeAgentCi.GENERIC_FANOUT_RECONCILE_CONFIG_SCHEMA, 'homeboy/generic-fanout-reconcile-config/v1');
assert.equal(runtimeAgentCi.FANOUT_RECONCILE_PLAN_SCHEMA, 'homeboy/fanout-reconcile-plan/v1');
assert.equal(runtimeAgentCi.createGenericFanoutReconcilePlan, genericWorkflow.createGenericFanoutReconcilePlan);
assert.equal(runtimeAgentCi.createFanoutReconcilePlan, runner.createFanoutReconcilePlan);

const loadedAfterImport = Object.keys(require.cache)
  .filter((modulePath) => !before.has(modulePath))
  .map((modulePath) => path.relative(repoRoot, modulePath).replace(/\\/g, '/'));

const forbiddenLoadedModules = loadedAfterImport.filter((modulePath) => (
  modulePath.startsWith('wordpress/') || modulePath.startsWith('agent-runtimes/wp-codebox/')
));
assert.deepEqual(
  forbiddenLoadedModules,
  [],
  `runtime-agent-ci generic fanout/reconcile imports must not load WordPress or WP Codebox modules: ${forbiddenLoadedModules.join(', ')}`,
);

const packageJson = JSON.parse(fs.readFileSync(path.join(repoRoot, 'runtime-agent-ci/package.json'), 'utf8'));
assert.equal(packageJson.main, 'index.js');
assert.equal(packageJson.exports['./generic-orchestration'], './generic-orchestration.js');
assert.equal(packageJson.exports['./provider-adapters'], './provider-adapters.js');
assert.equal(packageJson.exports['./controller-loop-proof-validator'], './lib/controller-loop-proof-validator.js');
assert.equal(packageJson.exports['./fanout-reconcile-runner'], './lib/fanout-reconcile-runner.js');
assert.equal(packageJson.exports['./generic-fanout-reconcile-workflow'], './lib/generic-fanout-reconcile-workflow.js');
assert.equal(packageJson.bin['homeboy-controller-loop-proof-validate'], './scripts/homeboy-controller-loop-proof-validate.cjs');
assert.equal(packageJson.bin['homeboy-generic-fanout-reconcile'], './scripts/homeboy-generic-fanout-reconcile.cjs');

const runtimeSources = [
  'runtime-agent-ci/index.js',
  'runtime-agent-ci/generic-orchestration.js',
  'runtime-agent-ci/provider-adapters.js',
  'runtime-agent-ci/lib/controller-loop-proof-validator.js',
  'runtime-agent-ci/lib/fanout-reconcile-runner.js',
  'runtime-agent-ci/lib/generic-fanout-reconcile-workflow.js',
  'runtime-agent-ci/scripts/homeboy-controller-loop-proof-validate.cjs',
  'runtime-agent-ci/scripts/homeboy-generic-fanout-reconcile.cjs',
];
const sourceViolations = runtimeSources.filter((relativePath) => /wordpress|wp-codebox|Codebox/.test(
  fs.readFileSync(path.join(repoRoot, relativePath), 'utf8')
));
assert.deepEqual(
  sourceViolations,
  [],
  `generic runtime-agent-ci fanout/reconcile sources must stay runtime-neutral: ${sourceViolations.join(', ')}`,
);

console.log('Homeboy generic fanout reconcile boundary test passed');
