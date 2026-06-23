'use strict';

/* eslint-disable camelcase */

/**
 * Internal dependencies
 */
const {
  createFanoutReconcilePlan,
  executeFanoutReconcileRun,
  groupFanoutItems,
} = require('./fanout-reconcile-runner');

const CONFIG_SCHEMA = 'homeboy/generic-fanout-reconcile-config/v1';
const RESULT_SCHEMA = 'homeboy/generic-fanout-reconcile-result/v1';
const FINDING_PACKET_CONFIG_SCHEMA = 'homeboy/generic-finding-packet-fanout-config/v1';

function createGenericFanoutReconcilePlan(input = {}) {
  const config = normalizeConfig(input.config || input);
  const items = normalizeItems(input.items || config.items || []);
  const groups = normalizeGroups(input.groups || config.groups, items, config);
  const orchestrator = config.orchestrator || {};

  return createFanoutReconcilePlan({
    schema: config.plan_schema || config.planSchema,
    orchestrator,
    groups,
    summary: {
      config_schema: config.schema || CONFIG_SCHEMA,
      item_count: groups.reduce((count, group) => count + group.items.length, 0),
      group_count: groups.length,
      ...(config.summary || {}),
    },
    render_task_request: (group) => renderTaskRequest(group, orchestrator, config),
    reconcile_plan: ({ groups: planGroups, task_requests }) => reconcileRecords({
      groups: planGroups,
      task_requests,
      records: [],
      config,
    }),
  });
}

function createFindingPacketFanoutPlan(input = {}) {
  const materialized = materializeFindingPacketFanoutConfig(input);
  return createGenericFanoutReconcilePlan({
    config: materialized.config,
    groups: materialized.groups,
  });
}

function materializeFindingPacketFanoutConfig(input = {}) {
  const baseConfig = normalizeConfig(input.config || {});
  const policy = normalizeFindingPolicy(input.policy || baseConfig.finding_policy || baseConfig.findingPolicy || {});
  const packets = input.packets || input.finding_packets || input.findingPackets || baseConfig.packets || baseConfig.finding_packets || [];
  const items = normalizeFindingPacketItems(packets, policy);
  const groups = groupFindingPacketItems(items, policy);
  const orchestrator = input.orchestrator || baseConfig.orchestrator || {};
  const taskRequestTemplate = input.task_request_template || input.taskRequestTemplate || baseConfig.task_request_template || baseConfig.taskRequestTemplate || defaultFindingPacketTaskTemplate(policy);

  return {
    config: {
      ...baseConfig,
      schema: baseConfig.schema || FINDING_PACKET_CONFIG_SCHEMA,
      orchestrator,
      task_request_template: taskRequestTemplate,
      runtime_execution: baseConfig.runtime_execution || baseConfig.runtimeExecution || baseConfig.runtime || input.runtime_execution || input.runtimeExecution || input.runtime,
      summary: {
        packet_count: countFindingPackets(packets),
        finding_count: items.length,
        grouping: {
          strategy: policy.group_key_template ? 'template' : 'paths',
          paths: policy.group_by,
        },
        ...(baseConfig.summary || {}),
      },
      finding_policy: policy,
    },
    groups,
    items,
  };
}

function createFindingPacketReconcileInput(input = {}) {
  const materialized = materializeFindingPacketFanoutConfig(input);
  return {
    config: materialized.config,
    plan: input.plan || createGenericFanoutReconcilePlan({
      config: materialized.config,
      groups: materialized.groups,
    }),
    records: normalizeFindingPacketRecords(input.records || []),
  };
}

async function createGenericFanoutReconcileResult(input = {}) {
  const config = normalizeConfig(input.config || {});
  const plan = input.plan || createGenericFanoutReconcilePlan(input);
  const records = normalizeRecords(input.records || []);
  const recordsById = new Map(records.map((record) => [taskId(record), record]));

  return executeFanoutReconcileRun({
    plan,
    concurrency: input.concurrency ?? config.concurrency,
    run_schema: config.result_schema || config.resultSchema || RESULT_SCHEMA,
    base_run: config.base_run || {},
    include_summary: config.include_summary !== false,
    task_id: taskId,
    execute_task_request: async (taskRequest) => recordsById.get(taskId(taskRequest)) || {
      id: taskId(taskRequest),
      group_key: taskRequest.group_key || '',
      status: 'missing_record',
    },
    classify_outcome: (record) => getPath(record, config.outcome_path || config.outcomePath || 'outcome'),
    is_record_successful: (record) => isRecordSuccessful(record, config),
    reconcile: ({ records: runRecords, outcomes }) => reconcileRecords({
      groups: plan.groups,
      task_requests: plan.task_requests,
      records: runRecords,
      outcomes,
      config,
    }),
  });
}

function normalizeConfig(config) {
  if (!config || typeof config !== 'object' || Array.isArray(config)) {
    return {};
  }
  return config;
}

function normalizeItems(items) {
  if (!Array.isArray(items)) {
    return [];
  }
  return items.map((item, index) => (item && typeof item === 'object' ? { ...item, index } : { value: item, index }));
}

function normalizeRecords(records) {
  if (!Array.isArray(records)) {
    return [];
  }
  return records.map((record, index) => (record && typeof record === 'object' ? record : { id: `record-${index + 1}`, value: record }));
}

function normalizeFindingPolicy(policy) {
  const normalized = normalizeConfig(policy);
  const groupBy = normalized.group_by || normalized.groupBy || normalized.group_key_paths || normalized.groupKeyPaths;
  return {
    packet_id_path: normalized.packet_id_path || normalized.packetIdPath || 'id',
    findings_path: normalized.findings_path || normalized.findingsPath || 'findings',
    finding_id_path: normalized.finding_id_path || normalized.findingIdPath || 'id',
    group_by: Array.isArray(groupBy) && groupBy.length > 0 ? groupBy : ['finding.type', 'finding.severity'],
    group_key_template: text(normalized.group_key_template || normalized.groupKeyTemplate),
    fallback_group_key: text(normalized.fallback_group_key || normalized.fallbackGroupKey) || 'default',
    include_packet: normalized.include_packet !== false,
  };
}

function normalizeFindingPacketItems(packets, policy = {}) {
  policy = normalizeFindingPolicy(policy);
  if (!Array.isArray(packets)) {
    return [];
  }

  return packets.flatMap((packet, packetIndex) => {
    const packetObject = packet && typeof packet === 'object' ? packet : { value: packet };
    const packetId = text(getPath(packetObject, policy.packet_id_path)) || text(packetObject.packet_id) || text(packetObject.key) || `packet-${packetIndex + 1}`;
    const findings = packetFindings(packetObject, policy);

    return findings.map((finding, findingIndex) => {
      const findingObject = finding && typeof finding === 'object' ? finding : { value: finding };
      const findingId = text(getPath(findingObject, policy.finding_id_path)) || text(findingObject.finding_id) || text(findingObject.rule_id) || text(findingObject.code) || `finding-${findingIndex + 1}`;
      const item = stripUndefined({
        id: `${packetId}:${findingId}`,
        packet_id: packetId,
        finding_id: findingId,
        index: findingIndex,
        packet_index: packetIndex,
        group_key: findingGroupKey(packetObject, findingObject, policy),
        type: text(findingObject.type) || text(findingObject.kind) || text(findingObject.rule_id),
        severity: text(findingObject.severity),
        path: text(findingObject.path) || text(findingObject.file),
        message: text(findingObject.message) || text(findingObject.description),
        finding: findingObject,
      });

      if (policy.include_packet) {
        item.packet = packetObject;
      }

      return item;
    });
  });
}

function packetFindings(packet, policy) {
  const configured = getPath(packet, policy.findings_path);
  if (Array.isArray(configured)) {
    return configured;
  }
  if (Array.isArray(packet.findings)) {
    return packet.findings;
  }
  if (Array.isArray(packet.diagnostics)) {
    return packet.diagnostics;
  }
  if (packet.finding && typeof packet.finding === 'object') {
    return [packet.finding];
  }
  return [];
}

function groupFindingPacketItems(items, policy = {}) {
  policy = normalizeFindingPolicy(policy);
  return groupFanoutItems(items, {
    group_key: (item) => text(item.group_key) || findingGroupKey(item.packet || {}, item.finding || item, policy),
  });
}

function findingGroupKey(packet, finding, policy = {}) {
  const context = { packet, finding };
  if (policy.group_key_template) {
    return text(renderString(policy.group_key_template, context)) || policy.fallback_group_key || 'default';
  }

  const parts = policy.group_by
    .map((pathExpression) => text(getPath(context, pathExpression)))
    .filter(Boolean);
  return parts.length > 0 ? parts.join(':') : policy.fallback_group_key || 'default';
}

function defaultFindingPacketTaskTemplate() {
  return {
    id: 'finding-packet-{{group.key}}',
    group_key: '{{group.key}}',
    item_ids: '{{group.item_ids}}',
    packet_ids: '{{group.packet_ids}}',
    finding_count: '{{group.item_count}}',
    inputs: {
      findings: '{{group.items}}',
    },
  };
}

function normalizeFindingPacketRecords(records) {
  return normalizeRecords(records).map((record) => stripUndefined({
    ...record,
    id: taskId(record),
    group_key: text(record.group_key) || text(record.groupKey),
    finding_ids: Array.isArray(record.finding_ids) ? record.finding_ids : undefined,
    packet_ids: Array.isArray(record.packet_ids) ? record.packet_ids : undefined,
  }));
}

function normalizeGroups(groups, items, config) {
  if (Array.isArray(groups) && groups.length > 0) {
    return groups.map((group, index) => ({
      key: text(group.key) || text(group.group_key) || text(group.id) || `group-${index + 1}`,
      index,
      items: normalizeItems(group.items || []),
    }));
  }

  const groupPath = config.group_key_path || config.groupKeyPath || config.group_by || config.groupBy || 'group_key';
  return groupFanoutItems(items, {
    group_key: (item) => text(getPath(item, groupPath)) || text(item.groupKey) || text(item.kind) || text(item.id) || 'default',
  });
}

function renderTaskRequest(group, orchestrator, config) {
  const template = config.task_request_template || config.taskRequestTemplate || {};
  const request = renderValue(template, { group, orchestrator, config });
  const id = text(request.id) || text(request.task_id) || stableTaskId(group, orchestrator);
  const runtime = config.runtime || config.runtime_execution || config.runtimeExecution || config.executor || {};

  return stripUndefined({
    id,
    group_key: text(request.group_key) || group.key,
    ...request,
    orchestrator: {
      ...orchestrator,
      group_index: group.index,
      ...(request.orchestrator || {}),
    },
    ...(Object.keys(runtime).length > 0 ? { runtime_execution: renderValue(runtime, { group, orchestrator, config }) } : {}),
  });
}

function reconcileRecords({ groups = [], task_requests = [], records = [], outcomes = [], config = {} }) {
  const successCount = records.filter((record) => isRecordSuccessful(record, config)).length;
  const failedRecords = records.filter((record) => !isRecordSuccessful(record, config));
  return {
    schema: config.reconciliation_schema || config.reconciliationSchema || 'homeboy/generic-fanout-reconcile-reconciliation/v1',
    group_count: groups.length,
    task_count: task_requests.length,
    record_count: records.length,
    success_count: successCount,
    failure_count: failedRecords.length,
    outcome_count: outcomes.length,
    failed_task_ids: failedRecords.map(taskId).filter(Boolean),
  };
}

function isRecordSuccessful(record, config = {}) {
  const successStatuses = Array.isArray(config.success_statuses) ? config.success_statuses : ['completed', 'success', 'passed'];
  if (record.success === true) {
    return true;
  }
  return successStatuses.includes(text(record.status));
}

function taskId(value) {
  return text(value.task_id) || text(value.id) || text(value.group_key);
}

function stableTaskId(group, orchestrator) {
  const prefix = text(orchestrator.run_id) || text(orchestrator.plan_id) || 'fanout';
  return `${prefix}-${group.key}`.replace(/[^a-zA-Z0-9._-]+/g, '-');
}

function renderValue(value, context) {
  if (typeof value === 'string') {
    return renderString(value, context);
  }
  if (Array.isArray(value)) {
    return value.map((entry) => renderValue(entry, context));
  }
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([key, entry]) => [key, renderValue(entry, context)]));
  }
  return value;
}

function renderString(value, context) {
  const exact = value.match(/^\{\{\s*([^}]+)\s*\}\}$/);
  if (exact) {
    return templateValue(exact[1], context);
  }
  return value.replace(/\{\{\s*([^}]+)\s*\}\}/g, (_match, expression) => stringifyTemplateValue(templateValue(expression, context)));
}

function templateValue(expression, context) {
  const path = expression.trim();
  if (path === 'group.item_count') {
    return context.group.items.length;
  }
  if (path === 'group.item_ids') {
    return context.group.items.map((item) => text(item.id) || text(item.key) || String(item.index + 1));
  }
  if (path === 'group.packet_ids') {
    return unique(context.group.items.map((item) => text(item.packet_id)).filter(Boolean));
  }
  if (path === 'group.finding_ids') {
    return context.group.items.map((item) => text(item.finding_id) || text(item.id) || String(item.index + 1));
  }
  return getPath(context, path);
}

function stringifyTemplateValue(value) {
  if (value === undefined || value === null) {
    return '';
  }
  if (typeof value === 'string') {
    return value;
  }
  return JSON.stringify(value);
}

function getPath(value, pathExpression) {
  if (!pathExpression) {
    return undefined;
  }
  return String(pathExpression).split('.').reduce((current, part) => {
    if (current === undefined || current === null) {
      return undefined;
    }
    return current[part];
  }, value);
}

function stripUndefined(value) {
  return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined));
}

function unique(values) {
  return Array.from(new Set(values));
}

function countFindingPackets(packets) {
  return Array.isArray(packets) ? packets.length : 0;
}

function text(value) {
  if (typeof value === 'string') {
    return value.trim();
  }
  if (typeof value === 'number' && Number.isFinite(value)) {
    return String(value);
  }
  return '';
}

module.exports = {
  CONFIG_SCHEMA,
  FINDING_PACKET_CONFIG_SCHEMA,
  RESULT_SCHEMA,
  createFindingPacketFanoutPlan,
  createFindingPacketReconcileInput,
  createGenericFanoutReconcilePlan,
  createGenericFanoutReconcileResult,
  groupFindingPacketItems,
  materializeFindingPacketFanoutConfig,
  normalizeFindingPacketItems,
  normalizeGroups,
  renderTaskRequest,
};
