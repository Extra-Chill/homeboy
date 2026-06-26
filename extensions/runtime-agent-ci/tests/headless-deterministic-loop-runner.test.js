'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
  runHeadlessDeterministicLoop,
  writeHeadlessDeterministicLoopArtifacts,
} = require('../lib/headless-deterministic-loop-runner');

const runtime = { id: 'fixture-runtime', executor: { backend: 'fixture', path: '/unused' } };
const baseSpec = {
  loop_id: 'headless-policy-fixture',
  runtime_id: 'fixture-runtime',
  runtime_profile: 'runtime-agent-ci',
  runtime_profiles: {
    'runtime-agent-ci': {
      id: 'runtime-agent-ci',
      runtime_task_ability: 'fixture/run',
    },
  },
  target_repo: 'Extra-Chill/example',
  component_path: '/workspace/example',
  task_id: 'build-site',
  workload_id: 'build-site',
};

(async () => {
const executedTaskIds = [];
const twoRevolution = await runHeadlessDeterministicLoop({
  spec: {
    ...baseSpec,
    loop_policy: {
      max_iterations: 3,
      accepted_statuses: ['succeeded'],
      continue_conditions: [{ outcome_status: 'failed', reason: 'validation_failed' }],
      validation_task: {
        task_id: 'validate-{{iteration}}',
        workload_id: 'validate-{{iteration}}',
        prompt: 'Validate {{previous_task_id}}',
      },
      repair_task_template: {
        task_id: 'repair-{{iteration}}',
        workload_id: 'repair-{{iteration}}',
        prompt: 'Repair after {{failure_summary}}',
      },
    },
  },
  runtime,
  validate: false,
  execute: ({ request }) => {
    executedTaskIds.push(request.task_id);
    if (request.task_id === 'validate-1') {
      return outcome(request, 'failed', 'Validation found drift.');
    }
    return outcome(request, 'succeeded', 'Task accepted.');
  },
});

assert.equal(twoRevolution.schema, 'homeboy/headless-deterministic-loop-result/v1');
assert.equal(twoRevolution.status, 'succeeded');
assert.deepEqual(executedTaskIds, ['build-site', 'validate-1', 'repair-2', 'validate-2']);
assert.equal(twoRevolution.tasks.length, 1);
assert.equal(twoRevolution.tasks[0].loop_policy.schema, 'homeboy/headless-loop-policy-status/v1');
assert.equal(twoRevolution.tasks[0].loop_policy.status, 'succeeded');
assert.equal(twoRevolution.tasks[0].loop_policy.stop_reason, 'accepted');
assert.equal(twoRevolution.tasks[0].loop_policy.iteration_count, 2);
assert.equal(twoRevolution.tasks[0].loop_policy.iterations[0].accepted, false);
assert.equal(twoRevolution.tasks[0].loop_policy.iterations[0].repair.required, true);
assert.equal(twoRevolution.tasks[0].loop_policy.iterations[0].repair.next_task_id, 'repair-2');
assert.equal(twoRevolution.tasks[0].loop_policy.iterations[1].candidate_task_id, 'repair-2');
assert.equal(twoRevolution.tasks[0].loop_policy.iterations[1].accepted, true);
assert.equal(twoRevolution.tasks[0].outcome.metadata.headless_loop_policy_status.iteration_count, 2);
assert.equal(twoRevolution.tasks[0].results.scenarios[0].metadata.completion_outcome_satisfied, true);

let boundedCalls = 0;
const boundedFailure = await runHeadlessDeterministicLoop({
  spec: {
    ...baseSpec,
    task_id: 'never-converges',
    workload_id: 'never-converges',
    loop_policy: {
      max_iterations: 2,
      accepted_statuses: ['succeeded'],
      continue_conditions: [{ outcome_status: 'failed' }],
      repair_task_template: {
        task_id: 'retry-{{iteration}}',
        workload_id: 'retry-{{iteration}}',
      },
    },
  },
  runtime,
  validate: false,
  execute: ({ request }) => {
    boundedCalls += 1;
    return outcome(request, 'failed', 'Still failing.');
  },
});

assert.equal(boundedCalls, 2);
assert.equal(boundedFailure.status, 'failed');
assert.equal(boundedFailure.tasks[0].loop_policy.status, 'failed');
assert.equal(boundedFailure.tasks[0].loop_policy.stop_reason, 'max_revolutions_reached');
assert.equal(boundedFailure.tasks[0].loop_policy.iteration_count, 2);

await assert.rejects(
  () => runHeadlessDeterministicLoop({
    spec: {
      ...baseSpec,
      task_id: 'duration-missing-sync-cap',
      workload_id: 'duration-missing-sync-cap',
      loop_policy: {
        mode: 'duration',
        duration_ms: 60_000,
        accepted_statuses: ['succeeded'],
        continue_conditions: [{ outcome_status: 'failed' }],
      },
    },
    runtime,
    validate: false,
    execute: ({ request }) => outcome(request, 'failed', 'Still failing.'),
  }),
  /require max_synchronous_revolutions/
);

let durationCalls = 0;
const durationBounded = await runHeadlessDeterministicLoop({
  spec: {
    ...baseSpec,
    task_id: 'duration-explicit-sync-cap',
    workload_id: 'duration-explicit-sync-cap',
    loop_policy: {
      mode: 'duration',
      duration_ms: 60_000,
      max_synchronous_revolutions: 2,
      accepted_statuses: ['succeeded'],
      continue_conditions: [{ outcome_status: 'failed' }],
    },
  },
  runtime,
  validate: false,
  now: () => 1000,
  execute: ({ request }) => {
    durationCalls += 1;
    return outcome(request, 'failed', 'Still failing.');
  },
});

assert.equal(durationCalls, 2);
assert.equal(durationBounded.status, 'failed');
assert.equal(durationBounded.tasks[0].loop_policy.stop_reason, 'max_revolutions_reached');
assert.equal(durationBounded.tasks[0].loop_policy.iteration_count, 2);

let expiredHeadlessCalls = 0;
const expiredHeadlessDeadline = await runHeadlessDeterministicLoop({
  spec: {
    ...baseSpec,
    task_id: 'expired-headless-deadline',
    workload_id: 'expired-headless-deadline',
    loop_policy: {
      mode: 'duration',
      deadline_at: 2000,
      max_synchronous_revolutions: 2,
      accepted_statuses: ['succeeded'],
    },
  },
  runtime,
  validate: false,
  now: () => 2000,
  execute: ({ request }) => {
    expiredHeadlessCalls += 1;
    return outcome(request, 'succeeded', 'Should not run.');
  },
});

assert.equal(expiredHeadlessCalls, 0);
assert.equal(expiredHeadlessDeadline.status, 'failed');
assert.equal(expiredHeadlessDeadline.tasks[0].loop_policy.stop_reason, 'deadline_reached');
assert.equal(expiredHeadlessDeadline.tasks[0].loop_policy.iteration_count, 0);

const multiTaskExecutionOrder = [];
const multiTask = await runHeadlessDeterministicLoop({
  spec: {
    ...baseSpec,
    loop_id: 'headless-multi-task-fixture',
    tasks: [
      { task_id: 'task-b', workload_id: 'task-b' },
      { task_id: 'task-a', workload_id: 'task-a' },
    ],
    task_concurrency: 2,
  },
  runtime,
  validate: false,
  execute: ({ request }) => {
    multiTaskExecutionOrder.push(request.task_id);
    return outcome(request, request.task_id === 'task-a' ? 'succeeded' : 'failed', 'Fanout task completed.');
  },
});

assert.deepEqual(multiTaskExecutionOrder, ['task-b', 'task-a']);
assert.equal(multiTask.status, 'failed');
assert.deepEqual(multiTask.tasks.map((task) => task.task_id), ['task-b', 'task-a']);
assert.deepEqual(multiTask.fanout.records.map((record) => record.id), ['task-b', 'task-a']);
assert.equal(multiTask.outcome.task_id, 'task-a');

const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-headless-loop-policy-'));
try {
  const loopPolicyFile = path.join(tmpRoot, 'loop-policy.json');
  const statusFile = path.join(tmpRoot, 'status.json');
  const loopResultFile = path.join(tmpRoot, 'loop-result.json');
  writeHeadlessDeterministicLoopArtifacts({
    result: twoRevolution,
    loopPolicyFile,
    statusFile,
    loopResultFile,
  });
  const loopPolicyArtifact = JSON.parse(fs.readFileSync(loopPolicyFile, 'utf8'));
  const statusArtifact = JSON.parse(fs.readFileSync(statusFile, 'utf8'));
  const loopResultArtifact = JSON.parse(fs.readFileSync(loopResultFile, 'utf8'));
  assert.equal(loopPolicyArtifact.schema, 'homeboy/headless-loop-policy-artifact/v1');
  assert.equal(loopPolicyArtifact.tasks[0].loop_policy.iteration_count, 2);
  assert.equal(statusArtifact.schema, 'homeboy/headless-deterministic-loop-status/v1');
  assert.equal(statusArtifact.status, 'succeeded');
  assert.equal(loopResultArtifact.schema, 'homeboy/headless-deterministic-loop-result/v1');

  const sharedRunDir = path.join(tmpRoot, 'shared-run');
  writeHeadlessDeterministicLoopArtifacts({
    result: twoRevolution,
    artifact_paths: { run_dir: sharedRunDir },
  });
  assert.equal(JSON.parse(fs.readFileSync(path.join(sharedRunDir, 'events.json'), 'utf8'))[0].type, 'loop_started');
  assert.equal(JSON.parse(fs.readFileSync(path.join(sharedRunDir, 'status.json'), 'utf8')).status, 'succeeded');
  assert.equal(JSON.parse(fs.readFileSync(path.join(sharedRunDir, 'results.json'), 'utf8')).scenarios[0].id, 'build-site');
} finally {
  fs.rmSync(tmpRoot, { recursive: true, force: true });
}

function outcome(request, status, summary) {
  return {
    schema: 'homeboy/agent-task-outcome/v1',
    task_id: request.task_id,
    status,
    summary,
    metadata: {
      results: {
        scenarios: [{
          id: request.task_id,
          metrics: { generic_agent_task_executor_mean: status === 'succeeded' ? 1 : 0 },
          metadata: {
            job_status: status,
            success_status: status,
            completion_outcome: status,
            completion_outcome_satisfied: status === 'succeeded',
          },
        }],
      },
    },
  };
}

process.stdout.write('Headless deterministic loop policy checks passed\n');
})().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
