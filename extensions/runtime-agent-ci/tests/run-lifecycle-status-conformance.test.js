'use strict';

const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');

const {
  RUN_LIFECYCLE_FAILURE_STATUSES,
  RUN_LIFECYCLE_PENDING_STATUSES,
  RUN_LIFECYCLE_RETRYABLE_STATUSES,
  RUN_LIFECYCLE_STATUSES,
  RUN_LIFECYCLE_SUCCESS_STATUSES,
  classifyRunLifecycleStatus,
  normalizeRunLifecycleStatus,
} = require('../lib/runtime-status.cjs');

function homeboyNormalizeRunLifecycleStatus(status) {
  const command = process.env.HOMEBOY_COMMAND || 'homeboy';
  const result = spawnSync(command, ['contract', 'normalize', 'run-lifecycle-status', '--input', JSON.stringify(status)], {
    encoding: 'utf8',
  });

  if (result.error && result.error.code === 'ENOENT') {
    return { skipped: true, message: `${command} was not found` };
  }

  if (result.status !== 0) {
    const stderr = typeof result.stderr === 'string' ? result.stderr.trim() : '';
    if (/unrecognized subcommand/.test(stderr)) {
      return { skipped: true, message: `homeboy run-lifecycle-status normalizer is unavailable: ${stderr}` };
    }
    assert.fail(`homeboy contract normalize run-lifecycle-status failed for ${status}: ${stderr}`);
  }

  const payload = JSON.parse(result.stdout);
  assert.equal(payload.success, true, `homeboy normalized ${status}`);
  return payload.data;
}

const allKnownStatuses = [
  ...RUN_LIFECYCLE_PENDING_STATUSES,
  ...RUN_LIFECYCLE_SUCCESS_STATUSES,
  ...RUN_LIFECYCLE_FAILURE_STATUSES,
];

assert.deepEqual(allKnownStatuses, RUN_LIFECYCLE_STATUSES);
assert.deepEqual(RUN_LIFECYCLE_RETRYABLE_STATUSES.filter((status) => !RUN_LIFECYCLE_FAILURE_STATUSES.includes(status)), []);

for (const status of RUN_LIFECYCLE_STATUSES) {
  const expected = homeboyNormalizeRunLifecycleStatus(status);
  if (expected.skipped) {
    process.stdout.write(`run lifecycle status conformance skipped: ${expected.message}\n`);
    process.exit(0);
  }

  assert.equal(normalizeRunLifecycleStatus(status), status);
  assert.deepEqual(classifyRunLifecycleStatus(status), expected, `${status} classification matches Homeboy contract`);
}

assert.equal(normalizeRunLifecycleStatus('not-a-status'), 'unknown');

process.stdout.write('Run lifecycle status conformance check passed\n');
