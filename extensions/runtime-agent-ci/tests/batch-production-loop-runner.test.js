'use strict';

const assert = require('node:assert/strict');

const {
  BATCH_PRODUCTION_LOOP_EVIDENCE_SCHEMA,
  BATCH_PRODUCTION_LOOP_RESULT_SCHEMA,
  BATCH_PRODUCTION_LOOP_WAVE_SCHEMA,
  batchProductionGroup,
  batchProductionTaskRequest,
  runBatchProductionLoop,
} = require('../lib/batch-production-loop-runner');

(async () => {
  assert.equal(BATCH_PRODUCTION_LOOP_RESULT_SCHEMA, 'homeboy/batch-production-loop-result/v1');
  assert.equal(BATCH_PRODUCTION_LOOP_WAVE_SCHEMA, 'homeboy/batch-production-loop-wave/v1');
  assert.equal(BATCH_PRODUCTION_LOOP_EVIDENCE_SCHEMA, 'homeboy/batch-production-loop-evidence/v1');
  assert.deepEqual(batchProductionGroup({ key: 'wpsg:theme' }, 0), { key: 'wpsg:theme' });
  assert.deepEqual(batchProductionTaskRequest({ key: 'wpsg:theme' }, 2), {
    schema: 'homeboy/batch-production-loop-task-request/v1',
    group: { key: 'wpsg:theme' },
    group_index: 2,
    group_key: 'wpsg:theme',
  });

  const successful = await runBatchProductionLoop({
    loopId: 'successful-batch',
    maxIterations: 3,
    concurrency: 2,
    planWave: () => ({ groups: [{ key: 'a' }, { key: 'b' }], evidence_refs: [{ kind: 'plan', url: 'https://example.test/plan' }] }),
    executeGroup: ({ group }) => ({ status: 'completed', group: group.key, evidence_refs: [{ kind: 'group', url: `https://example.test/${group.key}` }] }),
    reconcileWave: ({ groupOutcomes }) => ({ accepted: groupOutcomes.every((outcome) => outcome.success), evidence_refs: [{ kind: 'wave', url: 'https://example.test/wave' }] }),
  });

  assert.equal(successful.schema, BATCH_PRODUCTION_LOOP_RESULT_SCHEMA);
  assert.equal(successful.status, 'succeeded');
  assert.equal(successful.stop_reason, 'accepted');
  assert.equal(successful.wave_count, 1);
  assert.equal(successful.waves[0].schema, BATCH_PRODUCTION_LOOP_WAVE_SCHEMA);
  assert.equal(successful.waves[0].group_outcomes.length, 2);
  assert.equal(successful.evidence_envelope.schema, BATCH_PRODUCTION_LOOP_EVIDENCE_SCHEMA);
  assert.equal(successful.evidence_envelope.evidence_refs.length, 4);

  const runningCounts = [];
  let running = 0;
  const concurrencyLimited = await runBatchProductionLoop({
    loopId: 'concurrency-limited',
    concurrency: 2,
    planWave: () => [{ key: 'a' }, { key: 'b' }, { key: 'c' }, { key: 'd' }],
    executeGroup: async () => {
      running += 1;
      runningCounts.push(running);
      await new Promise((resolve) => setTimeout(resolve, 5));
      running -= 1;
      return { status: 'completed' };
    },
  });

  assert.equal(concurrencyLimited.status, 'succeeded');
  assert.equal(Math.max(...runningCounts), 2);

  const attempts = new Map();
  const plannedRetryGroups = [];
  const partialRetry = await runBatchProductionLoop({
    loopId: 'partial-retry',
    maxIterations: 3,
    concurrency: 3,
    planWave: ({ iteration, retryGroups }) => {
      plannedRetryGroups.push(retryGroups.map((group) => group.key));
      return iteration === 1 ? [{ key: 'a' }, { key: 'b' }, { key: 'c' }] : { groups: retryGroups };
    },
    executeGroup: ({ group }) => {
      const attempt = (attempts.get(group.key) || 0) + 1;
      attempts.set(group.key, attempt);
      if (group.key === 'b' && attempt === 1) {
        return { status: 'failed' };
      }
      return { status: 'completed' };
    },
    reconcileWave: ({ groupOutcomes, failedGroups }) => ({
      accepted: failedGroups.length === 0,
      retry_groups: failedGroups,
      state: { latest_failed_count: failedGroups.length },
      evidence_refs: [{ kind: 'reconcile', url: `https://example.test/reconcile/${groupOutcomes.length}` }],
    }),
  });

  assert.equal(partialRetry.status, 'succeeded');
  assert.equal(partialRetry.wave_count, 2);
  assert.deepEqual(plannedRetryGroups, [[], ['b']]);
  assert.equal(partialRetry.waves[0].failed_group_count, 1);
  assert.deepEqual(partialRetry.waves[1].groups.map((group) => group.key), ['b']);
  assert.equal(partialRetry.final_state.latest_failed_count, 0);

  let repairCalls = 0;
  let fanoutCalls = 0;
  const hookRun = await runBatchProductionLoop({
    loopId: 'hooks',
    maxIterations: 2,
    planWave: ({ iteration, retryGroups }) => iteration === 1 ? [{ key: 'root' }] : retryGroups,
    fanoutPolicy: ({ groups }) => {
      fanoutCalls += 1;
      return { groups: groups.flatMap((group) => group.key === 'root' ? [{ key: 'x' }, { key: 'y' }] : [group]) };
    },
    executeGroup: ({ group }) => ({ status: group.key === 'x' ? 'failed' : 'completed' }),
    reconcileWave: ({ failedGroups }) => ({ accepted: failedGroups.length === 0 }),
    repairPolicy: ({ failedGroups }) => {
      repairCalls += 1;
      return { retry_groups: failedGroups.map((group) => ({ ...group, key: 'y' })) };
    },
  });

  assert.equal(hookRun.status, 'succeeded');
  assert.equal(hookRun.wave_count, 2);
  assert.equal(fanoutCalls, 2);
  assert.equal(repairCalls, 1);

  const boundedFailure = await runBatchProductionLoop({
    loopId: 'bounded-failure',
    maxIterations: 2,
    planWave: ({ retryGroups, iteration }) => iteration === 1 ? [{ key: 'a' }] : retryGroups,
    executeGroup: () => ({ status: 'failed' }),
  });

  assert.equal(boundedFailure.status, 'failed');
  assert.equal(boundedFailure.stop_reason, 'max_iterations_reached');
  assert.equal(boundedFailure.wave_count, 2);

  const injectedExecution = await runBatchProductionLoop({
    loopId: 'injected-execution',
    planWave: () => [{ key: 'a' }],
    execution: {
      runGroup: () => ({ status: 'completed' }),
    },
  });

  assert.equal(injectedExecution.status, 'succeeded');

  process.stdout.write('Batch production loop runner behavior checks passed\n');
})().catch((error) => {
  process.stderr.write(`${error.stack || error.message}\n`);
  process.exitCode = 1;
});
