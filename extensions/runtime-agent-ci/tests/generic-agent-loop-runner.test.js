'use strict';

const assert = require('node:assert/strict');

const genericLoopRunner = require('../lib/generic-agent-loop-runner');

assert.equal(
  Object.prototype.hasOwnProperty.call(genericLoopRunner, 'runDeterministicLoop'),
  false,
  'generic runtime adapter must not export deterministic loop internals'
);

const runtime = { id: 'fixture-runtime', executor: { backend: 'fixture', path: '/unused' } };
const plan = {
  workload_id: 'fixture-workload',
  target_repo: 'Extra-Chill/example',
  component_path: '/workspace/example',
  runtime_profiles: {
    'runtime-agent-ci': {
      id: 'runtime-agent-ci',
      runtime_task_ability: 'fixture/run',
    },
  },
  success_completion_outcomes: ['done'],
};
let executeCalls = 0;

const result = genericLoopRunner.runGenericAgentLoop({
  runtime,
  plan,
  validate: true,
  validationPolicy: { success_completion_outcomes: ['done'] },
  execute: ({ request }) => {
    executeCalls += 1;
    assert.equal(request.schema, 'homeboy/agent-task-request/v1');
    assert.equal(request.task_id, 'fixture-workload');
    return {
      schema: 'homeboy/agent-task-outcome/v1',
      task_id: request.task_id,
      status: 'succeeded',
      summary: 'Fixture executor completed.',
      metadata: {
        results: {
          scenarios: [{
            id: request.task_id,
            metrics: { generic_agent_task_executor_mean: 1 },
            metadata: {
              job_status: 'completed',
              success_status: 'no_changes',
              completion_outcome: 'done',
              completion_outcome_satisfied: true,
            },
          }],
        },
      },
    };
  },
});

assert.equal(executeCalls, 1);
assert.equal(result.request.task_id, 'fixture-workload');
assert.equal(result.outcome.status, 'succeeded');
assert.equal(result.results.scenarios[0].id, 'fixture-workload');
assert.equal(result.assertion.completion_outcome_satisfied, true);
assert.equal(result.loop.schema, 'homeboy/deterministic-loop-result/v1');
assert.equal(result.loop.status, 'completed');
assert.equal(result.loop.state.status, 'succeeded');
assert.equal(result.loop.iterations.length, 1);

process.stdout.write('Generic agent loop runner adapter delegation check passed\n');
