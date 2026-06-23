'use strict';

const assert = require('node:assert/strict');

const {
  evaluateLoopPolicy,
  loopPolicyMaxRevolutions,
  normalizeLoopPolicy,
} = require('../lib/loop-policy');
const {
  createDurableDeterministicLoop,
  runDeterministicLoop,
} = require('../lib/deterministic-loop-runner');

const countPolicy = normalizeLoopPolicy({ mode: 'count', max_iterations: 2 });
assert.equal(countPolicy.max_revolutions, 2);
assert.equal(evaluateLoopPolicy(countPolicy, { completed_revolutions: 1, now: 1000 }).reason, 'continue');
assert.equal(evaluateLoopPolicy(countPolicy, { completed_revolutions: 2, now: 1000 }).reason, 'max_revolutions_reached');

let executions = 0;
const counted = runDeterministicLoop({
  mode: 'count',
  max_revolutions: 2,
  execute: () => {
    executions += 1;
    return { status: 'succeeded' };
  },
  stopCriteria: () => false,
});
assert.equal(executions, 2);
assert.equal(counted.iterations.at(-1).stop.reason, 'max_revolutions_reached');

const durationPolicy = normalizeLoopPolicy({ mode: 'duration', duration_ms: 5000 });
assert.equal(evaluateLoopPolicy(durationPolicy, { started_at: 1000, now: 5999 }).reason, 'continue');
assert.equal(evaluateLoopPolicy(durationPolicy, { started_at: 1000, now: 6000 }).reason, 'duration_elapsed');
assert.equal(loopPolicyMaxRevolutions(durationPolicy), 1);
assert.throws(
  () => loopPolicyMaxRevolutions(durationPolicy, { requireNonCountMaxRevolutions: true }),
  /require max_synchronous_revolutions/
);
assert.equal(loopPolicyMaxRevolutions(durationPolicy, { requireNonCountMaxRevolutions: true, nonCountMaxRevolutions: 3 }), 3);

const deadlinePolicy = normalizeLoopPolicy({ mode: 'duration', deadline_at: 3000 });
assert.equal(evaluateLoopPolicy(deadlinePolicy, { now: 2999 }).reason, 'continue');
assert.equal(evaluateLoopPolicy(deadlinePolicy, { now: 3000 }).reason, 'deadline_reached');

const cancelledPolicy = normalizeLoopPolicy({ mode: 'indefinite', cancelled: true });
assert.equal(evaluateLoopPolicy(cancelledPolicy, { now: 1000 }).reason, 'cancelled');

assert.throws(
  () => runDeterministicLoop({
    mode: 'duration',
    duration_ms: 60_000,
    now: () => 1000,
    execute: () => ({ status: 'succeeded' }),
    stopCriteria: () => false,
  }),
  /require max_synchronous_revolutions/
);

let durationExecutions = 0;
const durationSync = runDeterministicLoop({
  mode: 'duration',
  duration_ms: 60_000,
  max_synchronous_revolutions: 2,
  now: () => 1000,
  execute: () => {
    durationExecutions += 1;
    return { status: 'succeeded' };
  },
  stopCriteria: () => false,
});
assert.equal(durationExecutions, 2);
assert.equal(durationSync.iterations.length, 2);

assert.throws(
  () => runDeterministicLoop({
    mode: 'indefinite',
    execute: () => ({ status: 'succeeded' }),
    stopCriteria: () => false,
  }),
  /require max_synchronous_revolutions/
);

let now = 1000;
const submitted = [];
const indefinite = createDurableDeterministicLoop({
  mode: 'indefinite',
  now: () => now,
  submitIteration: ({ iteration }) => {
    submitted.push(iteration);
    return { token: `indefinite-${iteration}` };
  },
  pollIteration: ({ token }) => ({ status: 'completed', outcome: { status: 'succeeded', metadata: { token } } }),
  reconcile: ({ state, outcome }) => ({ seen: [...(state.seen || []), outcome.metadata.token] }),
});

let state = indefinite.resume({ seen: [] });
assert.equal(state.current.token, 'indefinite-1');
state = indefinite.resume(state);
assert.equal(state.current, null);
assert.deepEqual(state.state.seen, ['indefinite-1']);
now = 2000;
state = indefinite.resume(state);
assert.equal(state.current.token, 'indefinite-2');
assert.deepEqual(submitted, [1, 2]);

process.stdout.write('Loop policy primitive checks passed\n');
