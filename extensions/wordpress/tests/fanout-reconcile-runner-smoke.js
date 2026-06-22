'use strict';

/* eslint-disable no-console */

/**
 * External dependencies
 */
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

/**
 * Internal dependencies
 */
const {
  createFanoutReconcilePlan,
  executeFanoutReconcileRun,
  groupFanoutItems,
} = require('../../runtime-agent-ci/lib/fanout-reconcile-runner');

function readJson(filePath) {
  return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

function renderTaskRequest(group, orchestrator) {
  return {
    id: `task-${group.key}`,
    group_key: group.key,
    item_ids: group.items.map((item) => item.id),
    goal: `Resolve ${group.key}: ${group.items.map((item) => item.id).join(', ')}`,
    orchestrator: {
      ...orchestrator,
      group_index: group.index,
    },
  };
}

function reconcilePlan({ groups }) {
  return {
    targets: groups.map((group) => ({
      group_key: group.key,
      item_ids: group.items.map((item) => item.id),
    })),
  };
}

async function main() {
  const items = [
    { id: 'a1', group: 'alpha' },
    { id: 'a2', group: 'alpha' },
    { id: 'b1', group: 'beta' },
  ];
  const groups = groupFanoutItems(items, { group_key: (item) => item.group });
  const plan = createFanoutReconcilePlan({
    schema: 'fixture/fanout-plan/v1',
    orchestrator: {
      id: 'fixture-orchestrator',
      run_id: 'fixture-run',
    },
    groups,
    summary: { fixture: true },
    render_task_request: renderTaskRequest,
    reconcile_plan: reconcilePlan,
  });
  const goldenPlan = readJson(path.join(__dirname, 'fixtures', 'fanout-reconcile-runner', 'golden-plan.json'));
  assert.deepEqual(plan, goldenPlan);

  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-fanout-reconcile-runner-'));
  const runsOutputPath = path.join(root, 'run.json');
  const progressEvents = [];
  try {
    const run = await executeFanoutReconcileRun({
      plan,
      concurrency: 2,
      runs_output_path: runsOutputPath,
      on_progress: (event) => progressEvents.push(event),
      execute_task_request: async (taskRequest) => ({
        id: taskRequest.id,
        group_key: taskRequest.group_key,
        item_ids: taskRequest.item_ids,
        status: taskRequest.group_key === 'beta' ? 'failed' : 'completed',
        outcome: {
          kind: taskRequest.group_key === 'beta' ? 'needs_review' : 'applied',
          item_ids: taskRequest.item_ids,
        },
      }),
      classify_outcome: (record) => record.outcome,
      is_record_successful: (record) => record.status === 'completed',
      reconcile: ({ records, outcomes }) => ({
        completed_item_ids: records
          .filter((record) => record.status === 'completed')
          .flatMap((record) => record.item_ids),
        review_item_ids: outcomes
          .filter((outcome) => outcome.kind === 'needs_review')
          .flatMap((outcome) => outcome.item_ids),
      }),
    });

    assert.equal(run.schema, 'homeboy/fanout-reconcile-run/v1');
    assert.equal(run.status, 'failed');
    assert.deepEqual(run.records.map((record) => record.id), ['task-alpha', 'task-beta']);
    assert.deepEqual(run.outcomes.map((outcome) => outcome.kind), ['applied', 'needs_review']);
    assert.deepEqual(run.reconciliation.completed_item_ids, ['a1', 'a2']);
    assert.deepEqual(run.reconciliation.review_item_ids, ['b1']);
    assert.equal(progressEvents.filter((event) => event.status === 'started').length, 2);
    assert.equal(progressEvents.find((event) => event.status === 'failed').group_key, 'beta');

    const persisted = readJson(runsOutputPath);
    assert.equal(persisted.status, 'failed');
    assert.equal(Object.hasOwn(persisted, 'current_group'), false);
    assert.deepEqual(persisted.reconciliation.review_item_ids, ['b1']);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }

  await assertRejectedTaskPersistsFailedRecord(plan);
  await assertTaskIdsAreRequiredAndUnique(plan);

  console.log('Homeboy fanout reconcile runner smoke passed');
}

async function assertRejectedTaskPersistsFailedRecord(plan) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-fanout-reconcile-runner-rejected-'));
  const runsOutputPath = path.join(root, 'run.json');
  const started = [];
  try {
    const run = await executeFanoutReconcileRun({
      plan,
      concurrency: 1,
      runs_output_path: runsOutputPath,
      on_progress: (event) => started.push(event),
      execute_task_request: async (taskRequest) => {
        if (taskRequest.group_key === 'alpha') {
          throw new Error('provider rejected alpha');
        }
        return {
          id: taskRequest.id,
          group_key: taskRequest.group_key,
          item_ids: taskRequest.item_ids,
          status: 'completed',
        };
      },
      is_record_successful: (record) => record.status === 'completed',
    });

    assert.equal(run.status, 'failed');
    assert.deepEqual(run.records.map((record) => record.id), ['task-alpha', 'task-beta']);
    assert.equal(run.records[0].status, 'failed');
    assert.equal(run.records[0].error_message, 'provider rejected alpha');
    assert.equal(run.records[1].status, 'completed');
    assert.equal(started.filter((event) => event.status === 'started').length, 2);

    const persisted = readJson(runsOutputPath);
    assert.equal(persisted.status, 'failed');
    assert.deepEqual(persisted.records.map((record) => record.status), ['failed', 'completed']);
    assert.equal(Object.hasOwn(persisted, 'current_group'), false);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
}

async function assertTaskIdsAreRequiredAndUnique(plan) {
  let executeCount = 0;
  await assert.rejects(
    executeFanoutReconcileRun({
      plan: {
        ...plan,
        task_requests: [
          { ...plan.task_requests[0], id: '', task_id: '', group_key: '' },
        ],
      },
      execute_task_request: async () => {
        executeCount += 1;
      },
    }),
    /non-empty task id/
  );
  assert.equal(executeCount, 0);

  await assert.rejects(
    executeFanoutReconcileRun({
      plan: {
        ...plan,
        task_requests: [
          { ...plan.task_requests[0], id: 'duplicate-task' },
          { ...plan.task_requests[1], id: 'duplicate-task' },
        ],
      },
      execute_task_request: async () => {
        executeCount += 1;
      },
    }),
    /unique task ids/
  );
  assert.equal(executeCount, 0);
}

main().catch((error) => {
  console.error(error && error.stack ? error.stack : String(error));
  process.exit(1);
});
