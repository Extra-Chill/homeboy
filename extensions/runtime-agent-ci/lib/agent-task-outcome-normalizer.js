'use strict';

const { AGENT_TASK_OUTCOME_SCHEMA } = require('../../agent-task-contracts');
const { RUN_OUTCOME_ENVELOPE_SCHEMA } = require('./runtime-contracts.cjs');
const { normalizeAgentTaskOutcomeStatus } = require('./runtime-status.cjs');

const TERMINAL_FAILURE_STATUSES = ['failed', 'provider_error', 'timeout', 'unable_to_remediate'];
const SUCCESS_STATUSES = ['succeeded', 'no_op', 'follow_up_issue'];

function normalizeAgentTaskOutcome(request, result = {}, options = {}) {
  if (!request || typeof request !== 'object' || Array.isArray(request)) {
    throw new TypeError('normalizeAgentTaskOutcome requires a request object.');
  }
  if (!request.task_id) {
    throw new Error('normalizeAgentTaskOutcome requires request.task_id.');
  }
  result = unwrapRunOutcomeEnvelope(result, request);

  const diagnostics = normalizeProviderDiagnostics(options.diagnostics ?? result.diagnostics);
  const status = normalizeAgentTaskStatus(result, options);
  const output = {
    schema: options.schema || AGENT_TASK_OUTCOME_SCHEMA,
    task_id: request.task_id,
    status,
    summary: options.summary || result.summary || result.message || defaultSummary(status, options.providerLabel),
    artifacts: normalizeProviderArtifacts(options.artifacts ?? result.artifacts),
    evidence_refs: normalizeProviderEvidenceRefs(options.evidenceRefs ?? result.evidence_refs),
    outputs: normalizeProviderOutputs(options.outputs ?? result.outputs),
    diagnostics,
    metadata: {
      ...normalizeProviderMetadata(result, options),
      ...runOutcomeEnvelopeMetadata(result),
    },
  };

  const failureClassification = options.failureClassification !== undefined
    ? options.failureClassification
    : providerFailureClassification(result.failure_classification, status);
  if (failureClassification) {
    output.failure_classification = failureClassification;
  }
  if (options.failureCode || result.failure_code) {
    output.failure_code = options.failureCode || result.failure_code;
  }

  const category = options.failureCategory || result.failure_category || agentTaskFailureCategory(result, diagnostics, failureClassification, status);
  if (category) {
    output.failure_category = category;
  }

  const retryable = options.retryable ?? result.retryable ?? agentTaskFailureRetryable(category, failureClassification, status);
  if (retryable !== undefined) {
    output.retryable = retryable;
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

  if (TERMINAL_FAILURE_STATUSES.includes(explicitStatus)) {
    return explicitStatus;
  }
  if (TERMINAL_FAILURE_STATUSES.includes(resultStatus)) {
    return resultStatus;
  }
  const status = explicitStatus || resultStatus;
  return normalizeAgentTaskOutcomeStatus(status === undefined ? result : { ...result, status }, { exitStatus });
}

function normalizeProviderStatus(result = {}, exitStatus = 0) {
  return normalizeAgentTaskOutcomeStatus(result, { exitStatus });
}

function providerFailureClassification(classification, status) {
  if (classification === 'provider' || classification === 'provider_quota' || classification === 'transient' || classification === 'timeout' || classification === 'policy_denied' || classification === 'capability_missing' || classification === 'invalid_input' || classification === 'execution_failed' || classification === 'unknown') {
    return classification;
  }
  if (classification === 'max_turns') {
    return 'timeout';
  }
  if (classification === 'runtime' || classification === 'task' || classification === 'incomplete') {
    return 'execution_failed';
  }
  if (classification) {
    return 'unknown';
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

function agentTaskFailureCategory(result = {}, diagnostics = [], failureClassification, status) {
  if (SUCCESS_STATUSES.includes(status)) {
    return undefined;
  }
  if (status === 'timeout' || failureClassification === 'timeout' || result.timeout) {
    return 'runtime.timeout';
  }

  const haystack = [
    result.failure_category,
    result.error_code,
    result.code,
    result.error,
    result.summary,
    result.message,
    ...diagnostics.flatMap((diagnostic) => [diagnostic.class, diagnostic.message, diagnostic.data?.code, diagnostic.data?.type]),
  ].filter(Boolean).join(' ').toLowerCase();

  if (/auth|oauth|credential|unauthori[sz]ed|forbidden|401|403/.test(haystack)) {
    return 'provider.auth';
  }
  if (/rate[ _-]?limit|too many requests|\b429\b/.test(haystack)) {
    return 'provider.rate_limit';
  }
  if (/quota|insufficient[_ -]?quota|billing|credit|spend limit/.test(haystack)) {
    return 'provider.quota';
  }
  if (/model|deployment|engine/.test(haystack) && /missing|not found|unknown|invalid|unsupported|unavailable/.test(haystack)) {
    return 'provider.model';
  }
  if (failureClassification === 'provider' || status === 'provider_error') {
    return 'provider.error';
  }
  if (failureClassification === 'transient') {
    return 'runtime.transient';
  }
  if (failureClassification === 'invalid_input') {
    return 'task.invalid_input';
  }
  if (failureClassification === 'policy_denied') {
    return 'task.policy_denied';
  }
  if (failureClassification === 'capability_missing') {
    return 'runtime.capability_missing';
  }
  if (failureClassification === 'execution_failed' || status === 'failed' || status === 'unable_to_remediate') {
    return 'runtime.execution_failed';
  }
  return undefined;
}

function agentTaskFailureRetryable(category, failureClassification, status) {
  if (!category && !failureClassification && !TERMINAL_FAILURE_STATUSES.includes(status)) {
    return undefined;
  }
  if (category === 'provider.auth' || category === 'provider.quota' || category === 'provider.model' || category === 'task.invalid_input' || category === 'task.policy_denied') {
    return false;
  }
  if (category === 'provider.rate_limit' || category === 'provider.error' || category === 'runtime.timeout' || category === 'runtime.transient' || failureClassification === 'transient' || status === 'timeout') {
    return true;
  }
  if (category === 'runtime.execution_failed' || category === 'runtime.capability_missing' || failureClassification === 'execution_failed' || failureClassification === 'capability_missing') {
    return false;
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

function runOutcomeEnvelopeMetadata(result) {
  if (!plainObject(result.metadata) || !plainObject(result.metadata.run_outcome_envelope)) {
    return {};
  }
  return Object.fromEntries(Object.entries({
    results: result.metadata.results,
    run_outcome_envelope: result.metadata.run_outcome_envelope,
  }).filter(([, value]) => value !== undefined));
}

function unwrapRunOutcomeEnvelope(result, request) {
  if (!plainObject(result) || result.schema !== RUN_OUTCOME_ENVELOPE_SCHEMA) {
    return result;
  }
  const outcome = plainObject(result.outcome) ? result.outcome : {};
  return {
    ...outcome,
    task_id: outcome.task_id || result.task_id || request.task_id,
    status: outcome.status || result.status,
    metadata: {
      ...(plainObject(outcome.metadata) ? outcome.metadata : {}),
      ...(Array.isArray(result.results?.scenarios) && !outcome.metadata?.results ? { results: result.results } : {}),
      run_outcome_envelope: Object.fromEntries(Object.entries({
        schema: result.schema,
        task_id: result.task_id,
        status: result.status,
        success: result.success,
        artifact_manifest: result.artifact_manifest,
        files: result.files,
      }).filter(([, value]) => value !== undefined && value !== '')),
    },
  };
}

module.exports = {
  agentTaskFailureCategory,
  agentTaskFailureRetryable,
  normalizeAgentTaskOutcome,
  normalizeAgentTaskStatus,
  normalizeProviderTaskOutcome,
  normalizeProviderStatus,
  providerFailureClassification,
};
