'use strict';

const { AGENT_TASK_REQUEST_SCHEMA } = require('../../../agent-task-contracts/agent-task-provider-contract');

const DELEGATED_RUN_REQUEST_SCHEMA = 'homeboy/delegated-run-request/v1';
const DELEGATED_RUN_RESULT_SCHEMA = 'homeboy/delegated-run-result/v1';
const DELEGATED_RUN_TYPES = ['command', 'agent_run'];
const DELEGATED_RUN_STATUSES = ['succeeded', 'failed', 'timeout', 'cancelled', 'provider_error'];

function normalizeDelegatedRunRequest(request, options = {}) {
  assertAgentTaskRequestShape(request);
  const config = request.executor?.config || {};
  const inputs = request.inputs || {};
  const declared = firstObject(
    options.delegated_run,
    options.delegatedRun,
    inputs.delegated_run,
    inputs.delegatedRun,
    config.delegated_run,
    config.delegatedRun
  );
  if (!declared) {
    return null;
  }

  const type = normalizeType(declared.type || declared.kind || declared.execution_type || declared.executionType);
  const execution = normalizeExecution(type, declared, request);
  return cleanObject({
    schema: DELEGATED_RUN_REQUEST_SCHEMA,
    id: declared.id || request.task_id,
    task_id: request.task_id,
    execution,
    instructions: declared.instructions || request.instructions || '',
    input: plainObject(declared.input) ? declared.input : {},
    workspace: plainObject(declared.workspace) ? declared.workspace : plainObject(request.workspace) ? request.workspace : {},
    limits: plainObject(declared.limits) ? declared.limits : plainObject(request.limits) ? request.limits : {},
    artifacts: Array.isArray(declared.artifacts) ? declared.artifacts : Array.isArray(request.expected_artifacts) ? request.expected_artifacts : [],
    metadata: sanitizeMetadata({
      ...(plainObject(declared.metadata) ? declared.metadata : {}),
      source_schema: request.schema,
      executor_backend: request.executor?.backend,
    }),
  });
}

function normalizeDelegatedRunResult(result = {}, options = {}) {
  const raw = plainObject(result) ? result : {};
  const status = normalizeStatus(options.status || raw.status, options.exitStatus ?? 0);
  return cleanObject({
    schema: DELEGATED_RUN_RESULT_SCHEMA,
    id: raw.id || options.id || raw.task_id || options.task_id || '',
    task_id: raw.task_id || options.task_id || '',
    status,
    summary: raw.summary || raw.message || defaultSummary(status),
    outputs: plainObject(raw.outputs) ? raw.outputs : {},
    artifacts: normalizeArtifacts(raw.artifacts),
    diagnostics: normalizeDiagnostics(raw.diagnostics),
    metadata: sanitizeMetadata(plainObject(raw.metadata) ? raw.metadata : {}),
  });
}

function assertAgentTaskRequestShape(request) {
  if (!request || typeof request !== 'object' || Array.isArray(request)) {
    throw new TypeError('Delegated run normalization requires an agent task request object.');
  }
  if (request.schema !== AGENT_TASK_REQUEST_SCHEMA) {
    throw new Error(`Delegated run normalization requires schema ${AGENT_TASK_REQUEST_SCHEMA}.`);
  }
  if (!request.task_id) {
    throw new Error('Delegated run normalization requires task_id.');
  }
}

function normalizeType(value) {
  const type = typeof value === 'string' ? value.trim() : '';
  if (!DELEGATED_RUN_TYPES.includes(type)) {
    throw new Error(`Delegated run type must be one of: ${DELEGATED_RUN_TYPES.join(', ')}.`);
  }
  return type;
}

function normalizeExecution(type, declared, request) {
  if (type === 'command') {
    const argv = normalizeArgv(declared.argv || declared.command);
    if (argv.length === 0) {
      throw new Error('Delegated command runs require command or argv.');
    }
    return cleanObject({
      type,
      argv,
      cwd: stringValue(declared.cwd),
      env_names: envNames(declared.env),
    });
  }

  const agent = stringValue(declared.agent || request.executor?.agent);
  const instructions = stringValue(declared.instructions || request.instructions);
  if (!agent && !instructions) {
    throw new Error('Delegated agent runs require agent or instructions.');
  }
  return cleanObject({
    type,
    agent,
    instructions,
    tools: normalizeList(declared.tools || request.tools),
  });
}

function normalizeArgv(value) {
  if (Array.isArray(value)) {
    return value.map(stringValue).filter(Boolean);
  }
  const command = stringValue(value);
  return command ? [command] : [];
}

function normalizeStatus(status, exitStatus) {
  if (DELEGATED_RUN_STATUSES.includes(status)) {
    return status;
  }
  if (status === 'completed') {
    return 'succeeded';
  }
  if (status === 'timed_out') {
    return 'timeout';
  }
  return exitStatus === 0 ? 'succeeded' : 'failed';
}

function normalizeArtifacts(value) {
  if (!Array.isArray(value)) {
    return [];
  }
  return value.filter(plainObject).map((artifact) => cleanObject({
    id: artifact.id || artifact.name || artifact.path || artifact.url,
    kind: artifact.kind || artifact.type || 'delegated-run-artifact',
    name: artifact.name,
    path: artifact.path,
    url: artifact.url,
    metadata: sanitizeMetadata(plainObject(artifact.metadata) ? artifact.metadata : {}),
  }));
}

function normalizeDiagnostics(value) {
  return (Array.isArray(value) ? value : []).filter(Boolean).map((diagnostic) => {
    if (typeof diagnostic === 'string') {
      return { class: 'delegated_run', message: diagnostic, data: {} };
    }
    return cleanObject({
      class: diagnostic.class || diagnostic.kind || diagnostic.code || 'delegated_run',
      message: diagnostic.message || String(diagnostic),
      data: sanitizeMetadata(plainObject(diagnostic.data) ? diagnostic.data : {}),
    });
  });
}

function sanitizeMetadata(value) {
  if (!plainObject(value)) {
    return {};
  }
  return Object.fromEntries(Object.entries(value)
    .filter(([key]) => !/secret|token|password|credential/i.test(key))
    .map(([key, entry]) => [key, sanitizeMetadataValue(entry)]));
}

function sanitizeMetadataValue(value) {
  if (Array.isArray(value)) {
    return value.map(sanitizeMetadataValue);
  }
  if (plainObject(value)) {
    return sanitizeMetadata(value);
  }
  return value;
}

function envNames(value) {
  if (!plainObject(value)) {
    return [];
  }
  return Object.keys(value).map(stringValue).filter(Boolean).sort();
}

function firstObject(...values) {
  return values.find(plainObject) || null;
}

function normalizeList(value) {
  return (Array.isArray(value) ? value : []).map(stringValue).filter(Boolean);
}

function stringValue(value) {
  return typeof value === 'string' ? value.trim() : '';
}

function plainObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value);
}

function cleanObject(value) {
  return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined && entry !== ''));
}

function defaultSummary(status) {
  return status === 'succeeded' ? 'Delegated run succeeded.' : 'Delegated run failed.';
}

module.exports = {
  DELEGATED_RUN_REQUEST_SCHEMA,
  DELEGATED_RUN_RESULT_SCHEMA,
  DELEGATED_RUN_STATUSES,
  DELEGATED_RUN_TYPES,
  normalizeDelegatedRunRequest,
  normalizeDelegatedRunResult,
};
