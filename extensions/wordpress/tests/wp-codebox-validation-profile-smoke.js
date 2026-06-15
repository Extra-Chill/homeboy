'use strict';

/* eslint-disable no-console */

/**
 * External dependencies
 */
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const manifestPath = path.join(__dirname, '..', 'wordpress.json');
const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));

const profiles = manifest.ci?.profiles || {};
const jobs = manifest.ci?.jobs || {};
const validation = profiles['wp-codebox-validation'];
const benchOnly = profiles['wp-codebox-validation-bench'];
const benchJob = jobs['wp-codebox-bench-offloaded'];

assert.ok(validation, 'Expected wp-codebox-validation profile.');
assert.deepEqual(
  validation.jobs,
  ['wp-codebox-build-smoke', 'wp-codebox-phpunit', 'wp-codebox-bench-offloaded'],
  'Validation profile should combine safe local smoke jobs with the remote-only bench job.'
);
assert.match(
  validation.summary,
  /Lab\/runner offload/,
  'Validation profile should explain that benchmark work is offloaded.'
);
assert.ok(
  validation.rerun_commands.some((command) => command.includes('/bench/local_execution')),
  'Validation profile should include a fail-closed local benchmark policy command.'
);
assert.ok(
  validation.rerun_commands.some((command) => command.includes('--runner <runner-id>')),
  'Validation profile should include an explicit runner rerun command.'
);
assert.match(
  validation.artifact_contract,
  /run artifacts/,
  'Validation profile should describe durable reviewer-facing artifacts.'
);

assert.deepEqual(
  benchOnly?.jobs,
  ['wp-codebox-bench-offloaded'],
  'Bench CI profile must contain exactly one bench job for homeboy bench --ci-profile.'
);
assert.equal(benchJob.command, 'bench', 'Offloaded job should remain a generic bench job.');
assert.equal(benchJob.fidelity, 'remote-only', 'Benchmark validation must be remote-only.');
assert.equal(benchJob.provider, 'homeboy-lab', 'Benchmark validation should route through Homeboy Lab.');
assert.match(
  benchJob.workflow,
  /--runner <runner-id>/,
  'Benchmark rerun workflow should require an explicit connected runner.'
);
assert.ok(
  benchJob.limitations.some((limitation) => limitation.includes('/bench/local_execution')),
  'Benchmark job should document the fail-closed local execution policy.'
);

console.log('wp-codebox validation profile smoke passed');
