'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
  createGenericFanoutReconcilePlan,
  createGenericFanoutReconcileResult,
} = require('../lib/generic-fanout-reconcile-workflow');
const { executeFanoutReconcileRun } = require('../lib/fanout-reconcile-runner');

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

  console.log('Generic fanout reconcile workflow test passed');
})().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
