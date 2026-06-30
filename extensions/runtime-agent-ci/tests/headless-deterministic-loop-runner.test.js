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
const loopArtifactRefDir = path.join(os.tmpdir(), `headless-loop-ref-${process.pid}`);
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
  artifact_paths: { run_dir: loopArtifactRefDir },
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
assert.equal(twoRevolution.tasks[0].loop_result.schema, 'homeboy/headless-loop-result-envelope/v1');
assert.equal(twoRevolution.tasks[0].loop_result.mode, 'count');
assert.equal(twoRevolution.tasks[0].loop_result.revolutions, 2);
assert.equal(twoRevolution.tasks[0].loop_result.artifact_manifest.schema, 'homeboy/runner-artifact-manifest-ref/v1');
assert.equal(twoRevolution.tasks[0].loop_result.artifact_manifest.manifest_schema, 'homeboy/artifact-manifest/v1');
assert.equal(twoRevolution.loop_result.schema, 'homeboy/headless-loop-result-envelope/v1');
assert.equal(twoRevolution.loop_result.revolutions, 2);
assert.equal(twoRevolution.loop_result.artifact_manifest.schema, 'homeboy/runner-artifact-manifest-ref/v1');
assert.equal(twoRevolution.tasks[0].outcome.metadata.headless_loop_policy_status.iteration_count, 2);
assert.equal(twoRevolution.tasks[0].results.scenarios[0].metadata.completion_outcome_satisfied, true);
assert.equal(twoRevolution.fanout.records[0].status, 'completed');
assert.equal(twoRevolution.fanout.records[0].outcome_status, 'succeeded');

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

let durationCalls = 0;
const durationBounded = await runHeadlessDeterministicLoop({
  spec: {
    ...baseSpec,
    task_id: 'duration-bounded',
    workload_id: 'duration-bounded',
    loop_policy: {
      mode: 'duration',
      duration_ms: 60_000,
      max_synchronous_revolutions: 2,
      accepted_statuses: ['succeeded'],
      continue_conditions: [{ outcome_status: 'failed' }],
      repair_task_template: {
        task_id: 'duration-retry-{{iteration}}',
        workload_id: 'duration-retry-{{iteration}}',
      },
    },
  },
  runtime,
  validate: false,
  now: () => 1000,
  execute: ({ request }) => {
    durationCalls += 1;
    return outcome(request, 'failed', 'Still failing inside duration window.');
  },
});

assert.equal(durationCalls, 2);
assert.equal(durationBounded.status, 'failed');
assert.equal(durationBounded.tasks[0].loop_policy.mode, 'duration');
assert.equal(durationBounded.tasks[0].loop_policy.stop_reason, 'max_synchronous_revolutions_reached');
assert.equal(durationBounded.tasks[0].loop_result.mode, 'duration');
assert.equal(durationBounded.tasks[0].loop_result.duration_ms, 60_000);

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

let expiredHeadlessCalls = 0;
const expiredDeadline = await runHeadlessDeterministicLoop({
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
assert.equal(expiredDeadline.status, 'failed');
assert.equal(expiredDeadline.tasks[0].loop_policy.stop_reason, 'deadline_reached');
assert.equal(expiredDeadline.tasks[0].loop_policy.iteration_count, 0);

const artifactDir = fs.mkdtempSync(path.join(os.tmpdir(), 'headless-loop-artifact-manifest-'));
try {
  writeHeadlessDeterministicLoopArtifacts({ result: twoRevolution, artifact_paths: { run_dir: artifactDir } });
  const manifest = JSON.parse(fs.readFileSync(path.join(artifactDir, 'homeboy-artifact-manifest.json'), 'utf8'));
  assert.equal(manifest.schema, 'homeboy/artifact-manifest/v1');
  assert.equal(manifest.artifacts.some((artifact) => artifact.path === 'loop-result.json'), true);
  assert.equal(manifest.artifacts.some((artifact) => artifact.path === 'events.json'), true);
  assert.equal(manifest.artifacts.every((artifact) => !path.isAbsolute(artifact.path)), true);
} finally {
  fs.rmSync(artifactDir, { recursive: true, force: true });
}

await assert.rejects(
  () => runHeadlessDeterministicLoop({
    spec: {
      ...baseSpec,
      task_id: 'until-stopped-without-durable-runtime',
      workload_id: 'until-stopped-without-durable-runtime',
      loop_policy: { mode: 'until_stopped' },
    },
    runtime,
    validate: false,
    execute: ({ request }) => outcome(request, 'succeeded', 'Should not run.'),
  }),
  /require durable loop runtime capability/
);

const durableSubmissions = [];
const untilStoppedCheckpoint = await runHeadlessDeterministicLoop({
  spec: {
    ...baseSpec,
    task_id: 'until-stopped-durable',
    workload_id: 'until-stopped-durable',
    loop_policy: { mode: 'until_stopped' },
  },
  runtime: { ...runtime, capabilities: ['durable_headless_loop'] },
  validate: false,
  submitIteration: ({ request, iteration }) => {
    durableSubmissions.push({ task_id: request.task_id, iteration });
    return { token: `durable-${iteration}` };
  },
  pollIteration: () => ({ status: 'running' }),
});
assert.equal(untilStoppedCheckpoint.status, 'checkpointed');
assert.deepEqual(durableSubmissions, [{ task_id: 'until-stopped-durable', iteration: 1 }]);
assert.equal(untilStoppedCheckpoint.tasks[0].loop_policy.status, 'checkpointed');
assert.equal(untilStoppedCheckpoint.tasks[0].loop_policy.mode, 'until_stopped');
assert.equal(untilStoppedCheckpoint.tasks[0].loop_policy.state.checkpoints.at(-1).type, 'submitted');
assert.equal(untilStoppedCheckpoint.tasks[0].loop_result.mode, 'until_stopped');

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

const dryRun = await runHeadlessDeterministicLoop({
  spec: baseSpec,
  runtime,
  dryRun: true,
});
assert.equal(dryRun.status, 'succeeded');
assert.equal(dryRun.tasks[0].outcome.status, 'no_op');
assert.equal(dryRun.fanout.status, 'completed');
assert.equal(dryRun.fanout.records[0].status, 'completed');
assert.equal(dryRun.fanout.records[0].outcome_status, 'no_op');

let controllerRequest = null;
const controllerBacked = await runHeadlessDeterministicLoop({
  spec: {
    ...baseSpec,
    task_id: 'controller-backed-loop',
    workload_id: 'controller-backed-loop',
    controller_execution: {
      spec: '.github/homeboy/controllers/static-site-generation-loop.controller.json',
      inputs: '.ci/controller-inputs.json',
      policy_result: '.ci/controller-policy.json',
      output: '.ci/controller-result.json',
      max_actions: 42,
      reconcile_stale: true,
      prepare: [{ argv: ['node', '.github/scripts/build-homeboy-controller-run-inputs.mjs'] }],
    },
  },
  runtime,
  validate: false,
  executeController: ({ request, controllerExecution }) => {
    controllerRequest = request;
    return {
      status: 'succeeded',
      summary: 'fixture controller succeeded',
      result: {
        schema: 'homeboy/agent-task-loop-controller-result/v1',
        loop_id: 'controller-backed-loop',
      },
      results: {
        scenarios: [{ id: request.task_id, metrics: { homeboy_controller_execution_mean: 1 }, metadata: { completion_outcome_satisfied: true } }],
      },
      controller_spec: controllerExecution.spec,
    };
  },
});
assert.equal(controllerBacked.status, 'succeeded');
assert.equal(controllerBacked.tasks[0].request.schema, 'homeboy/headless-controller-execution-request/v1');
assert.equal(controllerBacked.tasks[0].request.executor, undefined, 'controller tasks do not create runtime-package executor requests');
assert.equal(controllerBacked.tasks[0].outcome.metadata.controller_execution.max_actions, 42);
assert.equal(controllerBacked.tasks[0].outcome.metadata.controller_execution.reconcile_stale, true);
assert.equal(controllerBacked.tasks[0].outcome.metadata.controller_result.loop_id, 'controller-backed-loop');
assert.equal(controllerRequest.controller_execution.spec, '.github/homeboy/controllers/static-site-generation-loop.controller.json');

const controllerTmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-controller-execution-'));
try {
  const argvFile = path.join(controllerTmpRoot, 'argv.json');
  const fakeHomeboy = path.join(controllerTmpRoot, 'homeboy.js');
  fs.writeFileSync(fakeHomeboy, `#!/usr/bin/env node
const fs = require('node:fs');
const argv = process.argv.slice(2);
fs.writeFileSync(process.env.HOMEBOY_ARGV_FILE, JSON.stringify(argv));
const outputIndex = argv.indexOf('--output');
if (outputIndex !== -1) {
  fs.writeFileSync(argv[outputIndex + 1], JSON.stringify({ schema: 'fixture/controller-result', loop_id: 'controller-reconcile-loop' }));
}
`);
  fs.chmodSync(fakeHomeboy, 0o755);
  const defaultController = await runHeadlessDeterministicLoop({
    spec: {
      ...baseSpec,
      task_id: 'controller-reconcile-loop',
      workload_id: 'controller-reconcile-loop',
      component_path: controllerTmpRoot,
      controller_execution: {
        spec: 'controller.json',
        output: path.join(controllerTmpRoot, 'controller-result.json'),
        reconcile_stale: true,
        env: { HOMEBOY_ARGV_FILE: argvFile },
      },
    },
    runtime,
    validate: false,
    homeboyBin: fakeHomeboy,
  });
  assert.equal(defaultController.status, 'succeeded');
  assert.ok(JSON.parse(fs.readFileSync(argvFile, 'utf8')).includes('--reconcile-stale'));
  assert.equal(defaultController.tasks[0].outcome.metadata.controller_execution.reconcile_stale, true);
} finally {
  fs.rmSync(controllerTmpRoot, { recursive: true, force: true });
}

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
