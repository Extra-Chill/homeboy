'use strict';

const BATCH_PRODUCTION_LOOP_RESULT_SCHEMA = 'homeboy/batch-production-loop-result/v1';
const BATCH_PRODUCTION_LOOP_WAVE_SCHEMA = 'homeboy/batch-production-loop-wave/v1';
const BATCH_PRODUCTION_LOOP_EVIDENCE_SCHEMA = 'homeboy/batch-production-loop-evidence/v1';
const DEFAULT_CONCURRENCY = 3;
const DEFAULT_MAX_ITERATIONS = 3;

async function runBatchProductionLoop(options = {}) {
  const loopId = options.loopId || options.loop_id || 'batch-production-loop';
  const maxIterations = positiveInteger(options.maxIterations || options.max_iterations, DEFAULT_MAX_ITERATIONS);
  const concurrency = normalizeConcurrency(options.concurrency || options.maxConcurrency || options.max_concurrency);
  const planWave = requiredFunction(options.planWave || options.plan_wave, 'planWave');
  const executeGroup = resolveExecuteGroup(options);
  const reconcileWave = options.reconcileWave || options.reconcile_wave || defaultReconcileWave;
  const repairPolicy = options.repairPolicy || options.repair_policy || null;
  const fanoutPolicy = options.fanoutPolicy || options.fanout_policy || null;
  const classifyGroupOutcome = options.classifyGroupOutcome || options.classify_group_outcome || defaultClassifyGroupOutcome;
  const waves = [];
  let state = optionalObject(options.state || options.initialState || options.initial_state);
  let retryGroups = normalizeGroups(options.groups || options.initialGroups || options.initial_groups);
  let previousWave = null;
  let stopReason = '';

  for (let iteration = 1; iteration <= maxIterations; iteration += 1) {
    const plan = normalizeWavePlan(await planWave({
      loop_id: loopId,
      loopId,
      iteration,
      wave: iteration,
      state,
      previous_wave: previousWave,
      previousWave,
      retry_groups: retryGroups,
      retryGroups,
      waves,
    }), retryGroups);
    let groups = normalizeGroups(plan.groups);
    const fanout = await applyFanoutPolicy(fanoutPolicy, {
      loopId,
      loop_id: loopId,
      iteration,
      wave: iteration,
      state,
      plan,
      groups,
      previous_wave: previousWave,
      previousWave,
      retry_groups: retryGroups,
      retryGroups,
      waves,
    });
    groups = normalizeGroups(fanout.groups || groups);

    if (groups.length === 0) {
      stopReason = iteration === 1 ? 'empty_plan' : 'no_retry_groups';
      break;
    }

    const groupOutcomes = await executeGroups({
      loopId,
      iteration,
      groups,
      concurrency,
      executeGroup,
      classifyGroupOutcome,
      state,
      plan,
      fanout,
      options,
    });
    const failedGroups = groups.filter((group, index) => !groupOutcomes[index]?.success);
    const reconciliation = normalizeReconciliation(await reconcileWave({
      loop_id: loopId,
      loopId,
      iteration,
      wave: iteration,
      state,
      plan,
      fanout,
      groups,
      group_outcomes: groupOutcomes,
      groupOutcomes,
      failed_groups: failedGroups,
      failedGroups,
      previous_wave: previousWave,
      previousWave,
      waves,
    }));
    const accepted = reconciliation.accepted === true || reconciliation.complete === true || reconciliation.status === 'accepted' || reconciliation.status === 'succeeded';
    const repair = accepted ? { retry_groups: [], required: false } : await applyRepairPolicy(repairPolicy, {
      loop_id: loopId,
      loopId,
      iteration,
      wave: iteration,
      state,
      plan,
      fanout,
      groups,
      group_outcomes: groupOutcomes,
      groupOutcomes,
      failed_groups: failedGroups,
      failedGroups,
      reconciliation,
      previous_wave: previousWave,
      previousWave,
      waves,
    });
    const waveRecord = {
      schema: BATCH_PRODUCTION_LOOP_WAVE_SCHEMA,
      loop_id: loopId,
      iteration,
      wave: iteration,
      concurrency,
      plan,
      fanout,
      groups: groups.map(serializeGroup),
      group_outcomes: groupOutcomes,
      failed_group_count: failedGroups.length,
      accepted,
      reconciliation,
      repair,
      artifacts: collectRecords(plan.artifacts, fanout.artifacts, reconciliation.artifacts, repair.artifacts),
      evidence_refs: collectRecords(plan.evidence_refs, plan.evidence, fanout.evidence_refs, fanout.evidence, reconciliation.evidence_refs, reconciliation.evidence, repair.evidence_refs, repair.evidence),
    };

    waves.push(waveRecord);
    previousWave = waveRecord;
    state = {
      ...state,
      ...optionalObject(plan.state),
      ...optionalObject(reconciliation.state),
      ...optionalObject(repair.state),
      latest_wave: waveRecord,
      latest_group_outcomes: groupOutcomes,
    };

    if (accepted) {
      stopReason = reconciliation.stop_reason || 'accepted';
      break;
    }

    retryGroups = normalizeGroups(repair.retry_groups || repair.groups || reconciliation.retry_groups || reconciliation.groups || failedGroups);
    if (retryGroups.length === 0) {
      stopReason = reconciliation.stop_reason || 'no_retry_groups';
      break;
    }
    if (iteration === maxIterations) {
      stopReason = 'max_iterations_reached';
    }
  }

  const latestWave = waves[waves.length - 1] || null;
  const status = latestWave?.accepted ? 'succeeded' : 'failed';
  return {
    schema: BATCH_PRODUCTION_LOOP_RESULT_SCHEMA,
    loop_id: loopId,
    status,
    stop_reason: stopReason || (waves.length === 0 ? 'empty' : 'unknown'),
    max_iterations: maxIterations,
    concurrency,
    wave_count: waves.length,
    iteration_count: waves.length,
    waves,
    group_outcomes: waves.flatMap((wave) => wave.group_outcomes),
    evidence_envelope: buildEvidenceEnvelope({ loopId, status, waves }),
    final_state: state,
  };
}

async function executeGroups(context) {
  const records = new Array(context.groups.length);
  let nextIndex = 0;
  let running = 0;

  return new Promise((resolve) => {
    const startNext = () => {
      if (nextIndex >= context.groups.length && running === 0) {
        resolve(records);
        return;
      }
      while (running < context.concurrency && nextIndex < context.groups.length) {
        const index = nextIndex;
        const group = context.groups[index];
        nextIndex += 1;
        running += 1;
        Promise.resolve()
          .then(() => context.executeGroup({
            loop_id: context.loopId,
            loopId: context.loopId,
            iteration: context.iteration,
            wave: context.iteration,
            group,
            group_index: index,
            groupIndex: index,
            groups: context.groups,
            state: context.state,
            plan: context.plan,
            fanout: context.fanout,
            options: context.options,
          }))
          .then((outcome) => {
            records[index] = normalizeGroupOutcome({ group, index, outcome, classifyGroupOutcome: context.classifyGroupOutcome });
          })
          .catch((error) => {
            records[index] = failedGroupOutcome(group, index, error);
          })
          .finally(() => {
            running -= 1;
            startNext();
          });
      }
    };
    startNext();
  });
}

function resolveExecuteGroup(options) {
  const execution = optionalObject(options.execution || options.executor);
  return requiredFunction(options.executeGroup || options.execute_group || execution.executeGroup || execution.execute_group || execution.runGroup || execution.run_group, 'executeGroup');
}

async function applyFanoutPolicy(fanoutPolicy, context) {
  if (typeof fanoutPolicy !== 'function') {
    return { groups: context.groups };
  }
  const fanout = await fanoutPolicy(context);
  if (Array.isArray(fanout)) {
    return { groups: fanout };
  }
  return { groups: context.groups, ...optionalObject(fanout) };
}

async function applyRepairPolicy(repairPolicy, context) {
  if (typeof repairPolicy !== 'function') {
    return { required: context.failedGroups.length > 0, retry_groups: context.failedGroups };
  }
  const repair = await repairPolicy(context);
  if (Array.isArray(repair)) {
    return { required: repair.length > 0, retry_groups: repair };
  }
  return { required: normalizeGroups(repair?.retry_groups || repair?.groups).length > 0, ...optionalObject(repair) };
}

function defaultReconcileWave({ groupOutcomes }) {
  const complete = groupOutcomes.every((outcome) => outcome.success);
  return { status: complete ? 'succeeded' : 'failed', complete };
}

function defaultClassifyGroupOutcome(outcome) {
  const status = outcome?.status || outcome?.state || '';
  const success = outcome?.success === true || ['accepted', 'completed', 'passed', 'succeeded'].includes(status);
  return {
    success,
    status: status || (success ? 'completed' : 'failed'),
  };
}

function normalizeGroupOutcome({ group, index, outcome, classifyGroupOutcome }) {
  const classification = optionalObject(classifyGroupOutcome(outcome, group, index));
  const status = classification.status || outcome?.status || outcome?.state || (classification.success ? 'completed' : 'failed');
  return {
    group_key: groupKey(group, index),
    group_index: index,
    status,
    success: classification.success === true,
    outcome,
    artifacts: collectRecords(outcome?.artifacts),
    evidence_refs: collectRecords(outcome?.evidence_refs, outcome?.evidence),
  };
}

function failedGroupOutcome(group, index, error) {
  return {
    group_key: groupKey(group, index),
    group_index: index,
    status: 'failed',
    success: false,
    outcome: null,
    artifacts: [],
    evidence_refs: [],
    error_message: errorMessage(error),
  };
}

function normalizeWavePlan(value, fallbackGroups) {
  if (Array.isArray(value)) {
    return { groups: value };
  }
  const plan = optionalObject(value);
  if (!Array.isArray(plan.groups) && fallbackGroups.length > 0) {
    return { ...plan, groups: fallbackGroups };
  }
  return { groups: [], ...plan };
}

function normalizeReconciliation(value) {
  if (value === true) {
    return { accepted: true, status: 'accepted' };
  }
  if (value === false) {
    return { accepted: false, status: 'failed' };
  }
  return optionalObject(value);
}

function buildEvidenceEnvelope({ loopId, status, waves }) {
  return {
    schema: BATCH_PRODUCTION_LOOP_EVIDENCE_SCHEMA,
    loop_id: loopId,
    status,
    wave_count: waves.length,
    group_count: waves.reduce((count, wave) => count + wave.groups.length, 0),
    failed_group_count: waves.reduce((count, wave) => count + wave.failed_group_count, 0),
    artifacts: collectRecords(...waves.map((wave) => wave.artifacts), ...waves.flatMap((wave) => wave.group_outcomes.map((outcome) => outcome.artifacts))),
    evidence_refs: collectRecords(...waves.map((wave) => wave.evidence_refs), ...waves.flatMap((wave) => wave.group_outcomes.map((outcome) => outcome.evidence_refs))),
    waves: waves.map((wave) => ({
      iteration: wave.iteration,
      accepted: wave.accepted,
      group_count: wave.groups.length,
      failed_group_count: wave.failed_group_count,
      artifacts: wave.artifacts,
      evidence_refs: wave.evidence_refs,
    })),
  };
}

function serializeGroup(group, index) {
  if (isPlainObject(group)) {
    return { key: groupKey(group, index), ...group };
  }
  return { key: groupKey(group, index), value: group };
}

function groupKey(group, index) {
  if (isPlainObject(group)) {
    return String(group.key || group.group_key || group.groupKey || group.id || index);
  }
  return String(index);
}

function normalizeGroups(value) {
  return Array.isArray(value) ? value : [];
}

function collectRecords(...recordSets) {
  const records = [];
  const seen = new Set();
  for (const recordSet of recordSets) {
    for (const record of normalizeGroups(recordSet)) {
      if (!record || typeof record !== 'object') {
        continue;
      }
      const key = record.url || record.uri || record.path || record.name || record.id || JSON.stringify(record);
      if (seen.has(key)) {
        continue;
      }
      seen.add(key);
      records.push(record);
    }
  }
  return records;
}

function normalizeConcurrency(value) {
  const parsed = positiveInteger(value, DEFAULT_CONCURRENCY);
  return Math.min(parsed, 16);
}

function positiveInteger(value, fallback) {
  const parsed = Number.parseInt(value || '', 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
}

function requiredFunction(value, name) {
  if (typeof value !== 'function') {
    throw new Error(`${name} must be a function.`);
  }
  return value;
}

function optionalObject(value) {
  return isPlainObject(value) ? value : {};
}

function isPlainObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value);
}

function errorMessage(error) {
  if (error && typeof error.message === 'string' && error.message.trim()) {
    return error.message;
  }
  return String(error || 'Group execution failed');
}

module.exports = {
  BATCH_PRODUCTION_LOOP_EVIDENCE_SCHEMA,
  BATCH_PRODUCTION_LOOP_RESULT_SCHEMA,
  BATCH_PRODUCTION_LOOP_WAVE_SCHEMA,
  DEFAULT_CONCURRENCY,
  runBatchProductionLoop,
};
