'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
  createGenericFanoutReconcilePlan,
  createGenericFanoutReconcileResult,
} = require('../lib/generic-fanout-reconcile-workflow');
const {
  AGENT_TASK_FANOUT_CANONICAL_PATH,
  AGENT_TASK_FANOUT_PLAN_SCHEMA,
  executeFanoutReconcileRun,
  projectHomeboyAgentTaskFanoutPlan,
} = require('../lib/fanout-reconcile-runner');

async function observedConcurrency(options = {}) {
  const plan = createGenericFanoutReconcilePlan({
    groups: Array.from({ length: 20 }, (_value, index) => ({
      key: `group-${index + 1}`,
      items: [{ id: `item-${index + 1}` }],
    })),
  });
  let active = 0;
  let maxActive = 0;
  const records = plan.task_requests.map((request) => ({
    id: request.id,
    group_key: request.group_key,
    then(resolve) {
      active += 1;
      maxActive = Math.max(maxActive, active);
      setImmediate(() => {
        active -= 1;
        resolve({
          id: request.id,
          group_key: request.group_key,
          status: 'completed',
        });
      });
    },
  }));

  const result = await createGenericFanoutReconcileResult({
    ...options,
    plan,
    records,
  });

  assert.equal(result.status, 'completed');
  assert.equal(result.records.length, plan.task_requests.length);

  return maxActive;
}

(async () => {
  assert.equal(await observedConcurrency(), 3, 'omitted concurrency should use the runner default');
  assert.equal(await observedConcurrency({ concurrency: 2 }), 2, 'input concurrency should be honored');
  assert.equal(await observedConcurrency({ config: { concurrency: 24 } }), 16, 'excessive config concurrency should be clamped by the runner');

  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-fanout-artifacts-'));
  try {
    const run = await executeFanoutReconcileRun({
      artifact_paths: { run_dir: tmpRoot },
      plan: {
        schema: 'homeboy/fanout-reconcile-plan/v1',
        task_requests: [{ task_id: 'fanout-artifact-task' }],
      },
      execute_task_request: (request) => ({ id: request.task_id, status: 'completed' }),
    });
    assert.equal(run.status, 'completed');
    assert.equal(JSON.parse(fs.readFileSync(path.join(tmpRoot, 'fanout-run.json'), 'utf8')).status, 'completed');
  } finally {
    fs.rmSync(tmpRoot, { recursive: true, force: true });
  }

  const providerStatusRun = await executeFanoutReconcileRun({
    plan: {
      schema: 'homeboy/fanout-reconcile-plan/v1',
      task_requests: [
        { task_id: 'provider-succeeded' },
        { task_id: 'provider-no-op' },
      ],
    },
    execute_task_request: (request) => ({
      id: request.task_id,
      status: request.task_id === 'provider-no-op' ? 'no_op' : 'succeeded',
      outcome: {
        schema: 'homeboy/agent-task-outcome/v1',
        task_id: request.task_id,
        status: request.task_id === 'provider-no-op' ? 'no_op' : 'succeeded',
      },
    }),
  });

  assert.equal(providerStatusRun.status, 'completed');
  assert.deepEqual(providerStatusRun.records.map((record) => record.status), ['completed', 'completed']);
  assert.deepEqual(providerStatusRun.records.map((record) => record.outcome.status), ['succeeded', 'no_op']);

  const homeboyFanoutPlan = {
    id: 'fanout/site-workflow',
    inputs: {
      schema: AGENT_TASK_FANOUT_PLAN_SCHEMA,
      fanout_id: 'fanout/site-workflow',
      plane: 'workflow',
      group_key: 'site-workflow',
      canonical_path: AGENT_TASK_FANOUT_CANONICAL_PATH,
      runtime_boundary: {
        boundary: 'manifest_declared_runtime_executor',
        durable_scheduler: 'homeboy',
        executor: 'declared_by_task_executor',
        runtime: 'declared_by_task_runtime',
      },
    },
    steps: [
      { id: 'generate', inputs: { request: { task_id: 'generate', instructions: 'Generate' } } },
      { id: 'diagnose', inputs: { request: { task_id: 'diagnose', instructions: 'Diagnose' } } },
    ],
  };
  const projected = projectHomeboyAgentTaskFanoutPlan(homeboyFanoutPlan);
  assert.equal(projected.plan_schema, AGENT_TASK_FANOUT_PLAN_SCHEMA);
  assert.equal(projected.orchestrator.canonical_path, AGENT_TASK_FANOUT_CANONICAL_PATH);
  assert.deepEqual(projected.task_requests.map((request) => request.task_id), ['generate', 'diagnose']);

  const homeboyProjectedRun = await executeFanoutReconcileRun({
    plan: homeboyFanoutPlan,
    execute_task_request: (request) => ({ id: request.task_id, group_key: request.group_key, status: 'completed' }),
  });
  assert.equal(homeboyProjectedRun.plan_schema, 'homeboy/fanout-reconcile-plan/v1');
  assert.equal(homeboyProjectedRun.orchestrator.fanout_id, 'fanout/site-workflow');
  assert.deepEqual(homeboyProjectedRun.records.map((record) => record.id), ['generate', 'diagnose']);

  console.log('Generic fanout reconcile workflow test passed');
})().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
