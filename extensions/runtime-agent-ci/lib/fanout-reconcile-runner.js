'use strict';

/**
 * External dependencies
 */
const fs = require('node:fs');
const path = require('node:path');
const { runtimeAgentArtifactPaths } = require('./artifact-paths.cjs');
const { normalizeHostRecordStatus, normalizeHostRunStatus } = require('./runtime-status.cjs');

const PLAN_SCHEMA = 'homeboy/fanout-reconcile-plan/v1';
const RUN_SCHEMA = 'homeboy/fanout-reconcile-run/v1';
const AGENT_TASK_FANOUT_PLAN_SCHEMA = 'homeboy/agent-task-fanout-plan/v1';
const AGENT_TASK_FANOUT_AGGREGATE_SCHEMA = 'homeboy/agent-task-fanout-aggregate/v1';
const AGENT_TASK_FANOUT_CANONICAL_PATH = 'homeboy-durable-scheduler-to-runtime-executor';
const AGENT_TASK_FANOUT_RUNTIME_BOUNDARY = 'manifest_declared_runtime_executor';
const FANOUT_RECONCILE_RECORD_STATUSES = ['completed', 'failed', 'missing_record'];
const FANOUT_RECONCILE_RUN_STATUSES = ['incomplete', 'completed', 'failed'];
const FANOUT_RECONCILE_SUCCESS_STATUSES = ['completed', 'succeeded', 'no_op', 'success', 'passed', 'accepted'];
const DEFAULT_CONCURRENCY = 3;

function writeJson(filePath, value) {
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, `${JSON.stringify(value, null, 2)}\n`);
}

function groupFanoutItems(items, options = {}) {
  const groupKey = typeof options.group_key === 'function'
    ? options.group_key
    : (item) => item.group_key || item.groupKey || item.kind || item.id || 'default';
  const groupsByKey = new Map();

  items.forEach((item, itemIndex) => {
    const key = String(groupKey(item, itemIndex) || 'default');
    if (!groupsByKey.has(key)) {
      groupsByKey.set(key, []);
    }
    groupsByKey.get(key).push(item);
  });

  return Array.from(groupsByKey.entries()).map(([key, groupItems], index) => ({
    key,
    index,
    items: groupItems,
  }));
}

function createFanoutReconcilePlan(input) {
  const items = Array.isArray(input.items) ? input.items : [];
  const groups = Array.isArray(input.groups) ? input.groups : groupFanoutItems(items, { group_key: input.group_key });
  const renderTaskRequest = requiredFunction(input.render_task_request, 'render_task_request');
  const reconcilePlan = typeof input.reconcile_plan === 'function' ? input.reconcile_plan : () => ({});
  const taskRequests = groups.map((group) => renderTaskRequest(group, input.orchestrator || {}));
  const reconciliation = reconcilePlan({
    groups,
    task_requests: taskRequests,
    orchestrator: input.orchestrator || {},
    context: input.context || {},
  }) || {};

  return {
    schema: input.schema || PLAN_SCHEMA,
    orchestrator: input.orchestrator || {},
    summary: {
      item_count: groups.reduce((count, group) => count + group.items.length, 0),
      group_count: groups.length,
      task_count: taskRequests.length,
      ...(input.summary || {}),
    },
    groups: groups.map((group) => ({
      key: group.key,
      index: group.index,
      item_count: group.items.length,
    })),
    task_requests: taskRequests,
    reconciliation,
  };
}

async function executeFanoutReconcileRun(input) {
  const plan = normalizeFanoutReconcilePlan(input.plan || createFanoutReconcilePlan(input));
  const executeTaskRequest = requiredFunction(input.execute_task_request, 'execute_task_request');
  const classifyOutcome = typeof input.classify_outcome === 'function' ? input.classify_outcome : (record) => record.outcome;
  const reconcile = typeof input.reconcile === 'function' ? input.reconcile : () => ({});
  const isRecordSuccessful = typeof input.is_record_successful === 'function'
    ? input.is_record_successful
    : (record) => record.status === 'completed' || record.success === true;
  const taskId = typeof input.task_id === 'function' ? input.task_id : defaultTaskId;
  const runningEntry = typeof input.running_entry === 'function' ? input.running_entry : defaultRunningEntry;
  const progressEvent = typeof input.progress_event === 'function' ? input.progress_event : defaultProgressEvent;
  const taskOrder = typeof input.task_order === 'function'
    ? input.task_order
    : (record) => defaultTaskOrder(plan, taskId, record);
  const onProgress = typeof input.on_progress === 'function' ? input.on_progress : () => {};
  const concurrency = normalizeConcurrency(input.concurrency);
  const taskIds = validateTaskIds(plan, taskId);
  const records = [];
  const running = new Map();
  const runSchema = input.run_schema || RUN_SCHEMA;
  const baseRun = {
    schema: runSchema,
    plan_schema: plan.schema,
    orchestrator: plan.orchestrator,
    ...(input.include_summary === false ? {} : { summary: plan.summary }),
    ...(input.base_run || {}),
  };
  const artifactPaths = runtimeAgentArtifactPaths(input);
  const writeRun = (run) => {
    if (artifactPaths.fanout_run) {
      writeJson(artifactPaths.fanout_run, run);
    }
  };
  const writeIncompleteRun = () => {
    writeRun({
      ...baseRun,
      records,
      status: normalizeHostRunStatus('incomplete'),
      current_group: firstRunningGroup(running),
      current_groups: runningGroups(running),
    });
  };

  writeRun({
    ...baseRun,
    records,
    status: normalizeHostRunStatus('incomplete'),
    current_group: null,
  });

  let nextIndex = 0;
  const startNext = () => {
    if (nextIndex >= plan.task_requests.length) {
      return false;
    }
    const taskRequest = plan.task_requests[nextIndex];
    const id = taskIds[nextIndex];
    nextIndex += 1;

    running.set(id, runningEntry(taskRequest));
    writeIncompleteRun();

    onProgress(progressEvent('started', taskRequest, plan));
    const promise = Promise.resolve()
      .then(() => executeTaskRequest(taskRequest, input))
      .catch((error) => failedTaskRecord(taskRequest, id, error))
      .then((record) => {
        record = normalizeFanoutRecord(record);
        records.push(record);
        records.sort((left, right) => taskOrder(left) - taskOrder(right));
        onProgress(progressEvent(record.status || (isRecordSuccessful(record) ? 'completed' : 'failed'), taskRequest, plan, record));
      })
      .finally(() => {
        running.delete(id);
        writeIncompleteRun();
        startNext();
      });
    running.get(id).promise = promise;
    return true;
  };

  while (running.size < concurrency && startNext()) {
    // Start the first batch.
  }

  while (running.size > 0) {
    await Promise.race(Array.from(running.values()).map((group) => group.promise));
  }

  const outcomes = records.flatMap((record) => {
    const outcome = classifyOutcome(record);
    return outcome ? [outcome] : [];
  });
  const reconciliation = reconcile({
    plan,
    records,
    outcomes,
  }) || {};
  const run = {
    ...baseRun,
    records,
    outcomes,
    ...(input.include_reconciliation === false ? {} : { reconciliation }),
    status: normalizeHostRunStatus({ success: records.every(isRecordSuccessful) }),
  };

  writeRun(run);

  return run;
}

function normalizeFanoutReconcilePlan(plan) {
  if (plan?.schema === AGENT_TASK_FANOUT_PLAN_SCHEMA || plan?.inputs?.schema === AGENT_TASK_FANOUT_PLAN_SCHEMA) {
    return projectHomeboyAgentTaskFanoutPlan(plan);
  }
  return plan;
}

function projectHomeboyAgentTaskFanoutPlan(plan) {
  const inputs = plan.inputs && typeof plan.inputs === 'object' ? plan.inputs : {};
  const fanoutId = text(plan.fanout_id || inputs.fanout_id || plan.id);
  const plane = text(plan.plane || inputs.plane || 'isolated_tasks') || 'isolated_tasks';
  const groupKey = text(plan.group_key || inputs.group_key || fanoutId);
  const tasks = Array.isArray(plan.tasks)
    ? plan.tasks
    : Array.isArray(plan.steps)
      ? plan.steps.map((step) => step?.inputs?.request || step?.request || step).filter(Boolean)
      : [];
  if (!fanoutId) {
    throw new Error('Homeboy agent-task fanout plan requires fanout_id');
  }
  if (tasks.length === 0) {
    throw new Error('Homeboy agent-task fanout plan requires at least one task');
  }
  const taskRequests = tasks.map((task, index) => {
    const taskId = text(task.task_id || task.id || task.name);
    if (!taskId) {
      throw new Error(`Homeboy agent-task fanout task at index ${index} requires task_id`);
    }
    return {
      ...task,
      task_id: taskId,
      id: task.id || taskId,
      group_key: text(task.group_key || groupKey || fanoutId),
      orchestrator: {
        ...(task.orchestrator || {}),
        fanout_id: fanoutId,
        plane,
        group_index: index,
      },
    };
  });

  return {
    schema: PLAN_SCHEMA,
    plan_schema: AGENT_TASK_FANOUT_PLAN_SCHEMA,
    orchestrator: {
      ...(plan.orchestrator || {}),
      fanout_id: fanoutId,
      plane,
      group_key: groupKey,
      canonical_path: text(inputs.canonical_path) || AGENT_TASK_FANOUT_CANONICAL_PATH,
      runtime_boundary: inputs.runtime_boundary || {
        boundary: AGENT_TASK_FANOUT_RUNTIME_BOUNDARY,
        durable_scheduler: 'homeboy',
        executor: 'declared_by_task_executor',
        runtime: 'declared_by_task_runtime',
      },
    },
    summary: {
      fanout_id: fanoutId,
      plane,
      task_count: taskRequests.length,
      group_count: taskRequests.length,
      item_count: taskRequests.length,
      source_schema: AGENT_TASK_FANOUT_PLAN_SCHEMA,
    },
    groups: taskRequests.map((taskRequest, index) => ({
      key: taskRequest.group_key,
      index,
      item_count: 1,
    })),
    task_requests: taskRequests,
    reconciliation: {
      schema: AGENT_TASK_FANOUT_AGGREGATE_SCHEMA,
      fanout_id: fanoutId,
      plane,
      status: 'planned',
    },
    homeboy_fanout_plan: plan,
  };
}

function requiredFunction(value, name) {
  if (typeof value !== 'function') {
    throw new Error(`fanout reconcile runner requires ${name}`);
  }
  return value;
}

function normalizeConcurrency(value) {
  const parsed = Number.parseInt(value || DEFAULT_CONCURRENCY, 10);
  if (!Number.isFinite(parsed) || parsed < 1) {
    return DEFAULT_CONCURRENCY;
  }
  return Math.min(parsed, 16);
}

function validateTaskIds(plan, taskId) {
  const seen = new Set();
  return plan.task_requests.map((taskRequest, index) => {
    const id = normalizeTaskId(taskId(taskRequest));
    if (!id) {
      throw new Error(`fanout reconcile runner requires a non-empty task id at index ${index}`);
    }
    if (seen.has(id)) {
      throw new Error(`fanout reconcile runner requires unique task ids; duplicate task id "${id}"`);
    }
    seen.add(id);
    return id;
  });
}

function normalizeTaskId(value) {
  if (typeof value === 'string') {
    return value.trim();
  }
  if (typeof value === 'number' && Number.isFinite(value)) {
    return String(value);
  }
  return '';
}

function failedTaskRecord(taskRequest, id, error) {
  return {
    id,
    group_key: taskRequest.group_key || '',
    status: 'failed',
    error_message: errorMessage(error),
  };
}

function normalizeFanoutRecord(record) {
  const normalized = record && typeof record === 'object' ? record : {};
  return {
    ...normalized,
    status: normalizeFanoutRecordStatus(normalized),
  };
}

function normalizeFanoutRecordStatus(record) {
  return normalizeHostRecordStatus(record);
}

function errorMessage(error) {
  if (error && typeof error.message === 'string' && error.message.trim()) {
    return error.message;
  }
  return String(error || 'Task execution failed');
}

function defaultTaskId(taskRequest) {
  return taskRequest.task_id || taskRequest.id || taskRequest.group_key;
}

function defaultRunningEntry(taskRequest) {
  return {
    id: defaultTaskId(taskRequest),
    group_key: taskRequest.group_key || '',
  };
}

function firstRunningGroup(running) {
  const group = runningGroups(running)[0];
  if (!group) {
    return null;
  }
  return group;
}

function runningGroups(running) {
  return Array.from(running.values()).map((group) => {
    const { promise, ...serializable } = group;
    return serializable;
  });
}

function defaultTaskOrder(plan, taskId, record) {
  const recordId = record.task_id || record.id || record.group_key;
  return plan.task_requests.findIndex((taskRequest) => taskId(taskRequest) === recordId);
}

function defaultProgressEvent(status, taskRequest, plan, record = null) {
  return {
    schema: 'homeboy/fanout-reconcile-progress/v1',
    status,
    id: defaultTaskId(taskRequest),
    group_key: taskRequest.group_key || '',
    group_index: Number(taskRequest.orchestrator?.group_index || 0) + 1,
    group_count: Number(plan.summary?.group_count || plan.task_requests.length || 0),
    outcome_kind: record?.outcome?.kind || '',
  };
}

function text(value) {
  if (value === undefined || value === null) {
    return '';
  }
  return String(value).trim();
}

module.exports = {
  PLAN_SCHEMA,
  RUN_SCHEMA,
  FANOUT_RECONCILE_PLAN_SCHEMA: PLAN_SCHEMA,
  AGENT_TASK_FANOUT_PLAN_SCHEMA,
  AGENT_TASK_FANOUT_AGGREGATE_SCHEMA,
  AGENT_TASK_FANOUT_CANONICAL_PATH,
  AGENT_TASK_FANOUT_RUNTIME_BOUNDARY,
  FANOUT_RECONCILE_RECORD_STATUSES,
  FANOUT_RECONCILE_SUCCESS_STATUSES,
  FANOUT_RECONCILE_RUN_SCHEMA: RUN_SCHEMA,
  FANOUT_RECONCILE_RUN_STATUSES,
  DEFAULT_CONCURRENCY,
  createFanoutReconcilePlan,
  executeFanoutReconcileRun,
  groupFanoutItems,
  normalizeFanoutReconcilePlan,
  projectHomeboyAgentTaskFanoutPlan,
  normalizeConcurrency,
};
