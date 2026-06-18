'use strict';

const DEFAULT_OUTCOME_SCHEMA = 'homeboy/agent-task-outcome/v1';

const TERMINAL_FAILURE_STATUSES = ['failed', 'provider_error', 'timeout', 'unable_to_remediate'];

function normalizeAgentTaskOutcome(request, result = {}, options = {}) {
  if (!request || typeof request !== 'object' || Array.isArray(request)) {
    throw new TypeError('normalizeAgentTaskOutcome requires a request object.');
  }
  if (!request.task_id) {
    throw new Error('normalizeAgentTaskOutcome requires request.task_id.');
  }

  const status = normalizeAgentTaskStatus(result, options);
  const output = {
    schema: options.schema || DEFAULT_OUTCOME_SCHEMA,
    task_id: request.task_id,
    status,
    summary: options.summary || result.summary || result.message || defaultSummary(status, options.providerLabel),
    artifacts: normalizeProviderArtifacts(options.artifacts ?? result.artifacts),
    evidence_refs: normalizeProviderEvidenceRefs(options.evidenceRefs ?? result.evidence_refs),
    outputs: normalizeProviderOutputs(options.outputs ?? result.outputs),
    diagnostics: normalizeProviderDiagnostics(options.diagnostics ?? result.diagnostics),
    metadata: normalizeProviderMetadata(result, options),
  };

  const failureClassification = options.failureClassification !== undefined
    ? options.failureClassification
    : providerFailureClassification(result.failure_classification, status);
  if (failureClassification) {
    output.failure_classification = failureClassification;
  }

  return output;
}

function normalizeProviderTaskOutcome(request, result = {}, options = {}) {
  return normalizeAgentTaskOutcome(request, result, options);
}

function normalizeAgentTaskStatus(result = {}, options = {}) {
  const exitStatus = options.exitStatus ?? 0;
  const explicitStatus = options.status;
  const resultStatus = result && typeof result === 'object' ? result.status : undefined;

  if (TERMINAL_FAILURE_STATUSES.includes(resultStatus)) {
    return resultStatus;
  }
  if (result?.provider_error) {
    return 'provider_error';
  }
  if (result?.timeout) {
    return 'timeout';
  }
  if (result?.unable_to_remediate) {
    return 'unable_to_remediate';
  }
  if (result?.success === false || exitStatus !== 0) {
    return 'failed';
  }
  if (TERMINAL_FAILURE_STATUSES.includes(explicitStatus)) {
    return explicitStatus;
  }
  if (explicitStatus) {
    return explicitStatus;
  }
  return normalizeProviderStatus(result, exitStatus);
}

function normalizeProviderStatus(result = {}, exitStatus = 0) {
  if (TERMINAL_FAILURE_STATUSES.includes(result.status)) {
    return result.status;
  }
  if (result.provider_error) {
    return 'provider_error';
  }
  if (result.timeout) {
    return 'timeout';
  }
  if (result.unable_to_remediate) {
    return 'unable_to_remediate';
  }
  if (result.success === false || exitStatus !== 0) {
    return 'failed';
  }
  if (result.status === 'completed') {
    return 'succeeded';
  }
  if (result.status === 'succeeded' || result.status === 'no_op') {
    return result.status;
  }
  if (result.outcome === 'no_op' || result.no_op) {
    return 'no_op';
  }
  if (result.success === true) {
    return 'succeeded';
  }
  return Object.keys(result || {}).length > 0 ? 'succeeded' : 'failed';
}

function providerFailureClassification(classification, status) {
  if (classification === 'provider' || classification === 'timeout') {
    return classification;
  }
  if (classification === 'runtime' || classification === 'task') {
    return 'execution_failed';
  }
  if (classification) {
    return classification;
  }
  if (status === 'provider_error') {
    return 'provider';
  }
  if (status === 'timeout') {
    return 'timeout';
  }
  if (status === 'failed' || status === 'unable_to_remediate') {
    return 'execution_failed';
  }
  return undefined;
}

function normalizeProviderArtifacts(value) {
  if (Array.isArray(value)) {
    return value.filter(Boolean);
  }
  if (typeof value === 'string' && value !== '') {
    return [{ id: 'provider-artifacts', kind: 'provider-artifact-directory', path: value }];
  }
  if (plainObject(value)) {
    return Object.entries(value).map(([name, artifact]) => normalizeProviderArtifact(name, artifact)).filter(Boolean);
  }
  return [];
}

function normalizeProviderArtifact(name, artifact) {
  if (typeof artifact === 'string' && artifact !== '') {
    return { id: name, kind: 'provider-artifact', path: artifact };
  }
  if (!plainObject(artifact)) {
    return null;
  }
  return Object.fromEntries(Object.entries({
    id: artifact.id || name,
    kind: artifact.kind || artifact.type || 'provider-artifact',
    path: artifact.path || artifact.directory || artifact.file,
    name: artifact.name,
    metadata: artifact.metadata,
  }).filter(([, entry]) => entry !== undefined && entry !== ''));
}

function normalizeProviderEvidenceRefs(value) {
  return Array.isArray(value) ? value.filter(Boolean) : [];
}

function normalizeProviderOutputs(value) {
  return plainObject(value) ? value : {};
}

function normalizeProviderDiagnostics(value) {
  return (Array.isArray(value) ? value : []).filter(Boolean).map((diagnostic) => ({
    class: diagnostic.class || diagnostic.kind || diagnostic.code || 'provider',
    message: diagnostic.message || String(diagnostic),
    data: plainObject(diagnostic.data) ? diagnostic.data : {},
  }));
}

function normalizeProviderMetadata(result, options = {}) {
  return Object.fromEntries(Object.entries({
    provider: options.provider,
    integration_contract: options.integrationContract,
    ...(plainObject(options.metadata) ? options.metadata : {}),
    provider_result: options.includeProviderResult === true ? result : undefined,
  }).filter(([, entry]) => entry !== undefined));
}

function defaultSummary(status, providerLabel = 'Provider') {
  return status === 'succeeded' || status === 'no_op'
    ? `${providerLabel} task succeeded.`
    : `${providerLabel} task failed.`;
}

function plainObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value);
}

module.exports = {
  normalizeAgentTaskOutcome,
  normalizeAgentTaskStatus,
  normalizeProviderTaskOutcome,
  normalizeProviderStatus,
  providerFailureClassification,
};
