'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const {
  createGenericFanoutReconcilePlan,
  createGenericFanoutReconcileResult,
} = require('../lib/generic-fanout-reconcile-workflow');

function readJson(filePath) {
  return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

function writeJson(filePath, value) {
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, `${JSON.stringify(value, null, 2)}\n`);
}

async function main() {
  const config = {
    schema: 'homeboy/generic-fanout-reconcile-config/v1',
    orchestrator: {
      id: 'fixture-orchestrator',
      run_id: 'fixture-run',
      plan_id: 'fixture-plan',
    },
    group_key_path: 'category',
    task_request_template: {
      id: 'task-{{group.key}}',
      group_key: '{{group.key}}',
      item_ids: '{{group.item_ids}}',
      item_count: '{{group.item_count}}',
      instructions: 'Process {{group.key}} with {{group.item_count}} item(s).',
      inputs: {
        items: '{{group.items}}',
      },
    },
    runtime_execution: {
      backend: 'caller-provided-runtime',
      task: {
        name: 'process-generic-group',
        group: '{{group.key}}',
      },
    },
  };
  const items = [
    { id: 'a1', category: 'alpha', payload: { value: 1 } },
    { id: 'a2', category: 'alpha', payload: { value: 2 } },
    { id: 'b1', category: 'beta', payload: { value: 3 } },
  ];
  const plan = createGenericFanoutReconcilePlan({ config, items });

  assert.equal(plan.schema, 'homeboy/fanout-reconcile-plan/v1');
  assert.deepEqual(plan.groups.map((group) => group.key), ['alpha', 'beta']);
  assert.deepEqual(plan.task_requests.map((request) => request.id), ['task-alpha', 'task-beta']);
  assert.deepEqual(plan.task_requests[0].item_ids, ['a1', 'a2']);
  assert.equal(plan.task_requests[0].item_count, 2);
  assert.equal(plan.task_requests[0].runtime_execution.backend, 'caller-provided-runtime');
  assert.equal(plan.reconciliation.record_count, 0);

  const result = await createGenericFanoutReconcileResult({
    config,
    plan,
    records: [
      { id: 'task-alpha', group_key: 'alpha', status: 'completed', outcome: { kind: 'processed', item_ids: ['a1', 'a2'] } },
      { id: 'task-beta', group_key: 'beta', status: 'needs_review', outcome: { kind: 'review', item_ids: ['b1'] } },
    ],
  });
  assert.equal(result.schema, 'homeboy/generic-fanout-reconcile-result/v1');
  assert.equal(result.status, 'failed');
  assert.deepEqual(result.records.map((record) => record.id), ['task-alpha', 'task-beta']);
  assert.equal(result.reconciliation.success_count, 1);
  assert.deepEqual(result.reconciliation.failed_task_ids, ['task-beta']);

  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-generic-fanout-reconcile-'));
  try {
    const configPath = path.join(root, 'config.json');
    const itemsPath = path.join(root, 'items.json');
    const recordsPath = path.join(root, 'records.json');
    const planPath = path.join(root, 'plan.json');
    const resultPath = path.join(root, 'result.json');
    writeJson(configPath, config);
    writeJson(itemsPath, items);
    writeJson(recordsPath, result.records);

    const cliPath = path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-generic-fanout-reconcile.cjs');
    const planRun = spawnSync(process.execPath, [cliPath, '--config', configPath, '--items', itemsPath, '--output', planPath], { encoding: 'utf8' });
    assert.equal(planRun.status, 0, planRun.stderr);
    assert.deepEqual(readJson(planPath), plan);

    const resultRun = spawnSync(process.execPath, [cliPath, '--config', configPath, '--plan', planPath, '--records', recordsPath, '--output', resultPath], { encoding: 'utf8' });
    assert.equal(resultRun.status, 0, resultRun.stderr);
    assert.equal(readJson(resultPath).reconciliation.failure_count, 1);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }

  console.log('Homeboy generic fanout reconcile workflow smoke passed');
}

main().catch((error) => {
  console.error(error && error.stack ? error.stack : String(error));
  process.exit(1);
});
