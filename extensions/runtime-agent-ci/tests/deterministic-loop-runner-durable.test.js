'use strict';

const assert = require('node:assert/strict');

const {
  DETERMINISTIC_LOOP_CHECKPOINT_SCHEMA,
  DETERMINISTIC_LOOP_DURABLE_STATE_SCHEMA,
  createDurableDeterministicLoop,
} = require('../lib/deterministic-loop-runner');

let now = 1000;
const submissions = [];
const pollStatuses = ['running', 'completed'];
const loop = createDurableDeterministicLoop({
  loopId: 'durable-fixture',
  maxIterations: 2,
  timeoutMs: 5000,
  now: () => now,
  state: { value: 0 },
  buildIteration: ({ state }) => ({ next: state.value + 1 }),
  submitIteration: ({ iteration, attempt, input }) => {
    const token = `job-${iteration}-${attempt}`;
    submissions.push({ iteration, attempt, input, token });
    return { token };
  },
  pollIteration: ({ token }) => {
    const status = pollStatuses.shift();
    if (status === 'running') {
      return { status };
    }
    return {
      status,
      outcome: {
        status: 'succeeded',
        metadata: { value: token === 'job-1-1' ? 1 : 2 },
        artifacts: [{ id: `${token}-evidence`, path: `/artifacts/${token}.json` }],
      },
    };
  },
  reconcile: ({ outcome }) => ({ value: outcome.metadata.value }),
  stopCriteria: ({ state }) => state.value === 2,
});

let state = loop.submitIteration({ value: 0 });
assert.equal(state.schema, DETERMINISTIC_LOOP_DURABLE_STATE_SCHEMA);
assert.equal(state.current.token, 'job-1-1');
assert.equal(state.current.deadline_at, 6000);
assert.equal(state.checkpoints[0].schema, DETERMINISTIC_LOOP_CHECKPOINT_SCHEMA);
assert.equal(state.checkpoints[0].type, 'submitted');

state = loop.pollIteration(state);
assert.equal(state.current.token, 'job-1-1');
assert.equal(state.iterations.length, 0);
assert.equal(state.checkpoints.at(-1).type, 'polled');
assert.equal(state.checkpoints.at(-1).data.status, 'running');

state = loop.resume(state);
assert.equal(state.current, null);
assert.equal(state.state.value, 1);
assert.equal(state.iterations.length, 1);
assert.equal(state.iterations[0].artifacts[0].path, '/artifacts/job-1-1.json');
assert.equal(state.checkpoints.at(-1).type, 'completed');

state = loop.resume(state);
assert.equal(state.current.token, 'job-2-1');
assert.deepEqual(submissions.map((entry) => entry.token), ['job-1-1', 'job-2-1']);

const retrySubmissions = [];
const retryLoop = createDurableDeterministicLoop({
  loopId: 'retry-fixture',
  maxAttempts: 2,
  backoffMs: 250,
  now: () => now,
  submitIteration: ({ iteration, attempt, retry_after_ms: retryAfterMs = 0 }) => {
    const token = `retry-${iteration}-${attempt}`;
    retrySubmissions.push({ token, retryAfterMs });
    return { token };
  },
  pollIteration: () => ({ status: 'completed', outcome: { status: 'failed', summary: 'retry me' } }),
  shouldRetry: ({ outcome }) => outcome.status === 'failed',
});

let retryState = retryLoop.submitIteration({});
now = 2000;
retryState = retryLoop.pollIteration(retryState);
assert.equal(retryState.current.token, 'retry-1-2');
assert.equal(retryState.current.submitted_at, 2250);
assert.equal(retryState.checkpoints.at(-1).type, 'retry_scheduled');
assert.deepEqual(retrySubmissions, [
  { token: 'retry-1-1', retryAfterMs: 0 },
  { token: 'retry-1-2', retryAfterMs: 250 },
]);

now = 5000;
const durationLoop = createDurableDeterministicLoop({
  mode: 'duration',
  durationMs: 500,
  now: () => now,
  submitIteration: () => ({ token: 'duration-1' }),
  pollIteration: () => ({ status: 'completed', outcome: { status: 'succeeded' } }),
});
let durationState = durationLoop.submitIteration({});
assert.equal(durationState.started_at, 5000);
now = 5600;
durationState = durationLoop.pollIteration(durationState);
assert.equal(durationState.done, true);
assert.equal(durationState.stop_reason, 'duration_elapsed');

now = 7000;
const deadlineLoop = createDurableDeterministicLoop({
  mode: 'duration',
  deadlineAt: 7000,
  now: () => now,
  submitIteration: () => ({ token: 'deadline-1' }),
  pollIteration: () => ({ status: 'completed', outcome: { status: 'succeeded' } }),
});
const deadlineState = deadlineLoop.submitIteration({});
assert.equal(deadlineState.started_at, 7000);
assert.equal(deadlineState.done, true);
assert.equal(deadlineState.stop_reason, 'deadline_reached');
assert.equal(deadlineState.current, null);

process.stdout.write('Durable deterministic loop resume and polling checks passed\n');
