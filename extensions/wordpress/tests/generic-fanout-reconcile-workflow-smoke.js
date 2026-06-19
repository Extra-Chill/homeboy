'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const {
  createFindingPacketFanoutPlan,
  createFindingPacketReconcileInput,
  createGenericFanoutReconcilePlan,
  createGenericFanoutReconcileResult,
  materializeFindingPacketFanoutConfig,
  normalizeFindingPacketItems,
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
      backend: 'caller-provided-executor',
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
  assert.equal(plan.task_requests[0].runtime_execution.backend, 'caller-provided-executor');
  assert.equal(Object.hasOwn(plan.task_requests[0], 'sandbox_session_id'), false);
  assert.equal(Object.hasOwn(plan.task_requests[0], 'wp_codebox_command'), false);
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

  const packets = [
    {
      id: 'packet-a',
      source: 'lint',
      findings: [
        { id: 'a1', type: 'syntax', severity: 'error', path: 'src/a.js', message: 'Unexpected token' },
        { id: 'a2', type: 'style', severity: 'warning', path: 'src/b.js', message: 'Use const' },
      ],
    },
    {
      id: 'packet-b',
      source: 'static-analysis',
      diagnostics: [
        { id: 'b1', type: 'syntax', severity: 'error', path: 'src/c.js', message: 'Missing semicolon' },
      ],
    },
  ];
  const packetPolicy = {
    group_by: ['finding.type', 'finding.severity'],
  };
  const packetItems = normalizeFindingPacketItems(packets, packetPolicy);
  assert.deepEqual(packetItems.map((item) => item.id), ['packet-a:a1', 'packet-a:a2', 'packet-b:b1']);
  assert.deepEqual(packetItems.map((item) => item.group_key), ['syntax:error', 'style:warning', 'syntax:error']);

  const materialized = materializeFindingPacketFanoutConfig({
    packets,
    policy: packetPolicy,
    orchestrator: { run_id: 'finding-run' },
  });
  assert.equal(materialized.config.schema, 'homeboy/generic-finding-packet-fanout-config/v1');
  assert.equal(materialized.config.summary.packet_count, 2);
  assert.equal(materialized.config.summary.finding_count, 3);
  assert.deepEqual(materialized.groups.map((group) => group.key), ['syntax:error', 'style:warning']);

  const templated = materializeFindingPacketFanoutConfig({
    packets,
    policy: {
      group_key_template: '{{packet.source}}/{{finding.severity}}',
    },
  });
  assert.deepEqual(templated.groups.map((group) => group.key), ['lint/error', 'lint/warning', 'static-analysis/error']);

  const packetPlan = createFindingPacketFanoutPlan({
    packets,
    policy: packetPolicy,
    orchestrator: { run_id: 'finding-run' },
  });
  assert.deepEqual(packetPlan.task_requests.map((request) => request.id), ['finding-packet-syntax:error', 'finding-packet-style:warning']);
  assert.deepEqual(packetPlan.task_requests[0].item_ids, ['packet-a:a1', 'packet-b:b1']);
  assert.deepEqual(packetPlan.task_requests[0].packet_ids, ['packet-a', 'packet-b']);
  assert.equal(packetPlan.task_requests[0].finding_count, 2);
  assert.deepEqual(packetPlan.task_requests[0].inputs.findings.map((finding) => finding.finding_id), ['a1', 'b1']);
  assert.equal(Object.hasOwn(packetPlan.task_requests[0], 'sandbox_session_id'), false);
  assert.equal(Object.hasOwn(packetPlan.task_requests[0], 'wp_codebox_command'), false);

  const reconcileInput = createFindingPacketReconcileInput({
    packets,
    policy: packetPolicy,
    plan: packetPlan,
    records: [
      { id: 'finding-packet-syntax:error', status: 'completed', outcome: { applied: ['packet-a:a1', 'packet-b:b1'] } },
      { id: 'finding-packet-style:warning', status: 'completed', outcome: { applied: ['packet-a:a2'] } },
    ],
  });
  assert.equal(reconcileInput.plan, packetPlan);
  assert.deepEqual(reconcileInput.records.map((record) => record.id), ['finding-packet-syntax:error', 'finding-packet-style:warning']);
  const packetResult = await createGenericFanoutReconcileResult(reconcileInput);
  assert.equal(packetResult.status, 'completed');
  assert.equal(packetResult.reconciliation.success_count, 2);

  console.log('Homeboy generic fanout reconcile workflow smoke passed');
}

main().catch((error) => {
  console.error(error && error.stack ? error.stack : String(error));
  process.exit(1);
});
