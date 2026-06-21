'use strict';

const assert = require('node:assert/strict');

const {
  DETERMINISTIC_LOOP_ARTIFACT_SCHEMA,
  DETERMINISTIC_LOOP_ITERATION_SCHEMA,
  DETERMINISTIC_LOOP_RESULT_SCHEMA,
  runDeterministicLoop,
} = require('../lib/deterministic-loop-runner');

assert.equal(DETERMINISTIC_LOOP_RESULT_SCHEMA, 'homeboy/deterministic-loop-result/v1');
assert.equal(DETERMINISTIC_LOOP_ITERATION_SCHEMA, 'homeboy/deterministic-loop-iteration/v1');
assert.equal(DETERMINISTIC_LOOP_ARTIFACT_SCHEMA, 'homeboy/deterministic-loop-artifact/v1');

const loop = runDeterministicLoop({
  loopId: 'incubation-boundary',
  maxIterations: 2,
  state: { value: 0 },
  buildIteration: ({ state }) => ({ next: state.value + 1 }),
  execute: ({ input }) => ({ status: 'succeeded', metadata: { value: input.next } }),
  reconcile: ({ outcome }) => ({ value: outcome.metadata.value }),
  stopCriteria: ({ state }) => state.value === 2,
});

assert.equal(loop.schema, DETERMINISTIC_LOOP_RESULT_SCHEMA);
assert.equal(loop.loop_id, 'incubation-boundary');
assert.equal(loop.status, 'completed');
assert.equal(loop.iterations.length, 2);
assert.equal(loop.state.value, 2);

process.stdout.write('Deterministic loop runner incubation boundary check passed\n');
